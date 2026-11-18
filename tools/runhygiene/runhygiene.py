#!/usr/bin/env python3
"""runhygiene: deterministic checks that make experiment scripts reproducible. Stdlib only, no model.

    python3 tools/runhygiene/runhygiene.py PATH... [--check] [--json] [--exclude GLOB]... [--baseline FILE] [--frozen LIST] [--base REF]

Findings carry kind, line and probability, as codehygiene's do. --check exits 1 when any finding is reported.
"""
import ast
import fnmatch
import json
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

THRESHOLD = 0.55
NP_PREFIXES = ("np.random.", "numpy.random.")
NP_GLOBAL = {"rand", "randn", "randint", "random", "choice", "shuffle", "permutation", "normal", "uniform"}
PY_GLOBAL = {"random", "randint", "choice", "shuffle", "sample", "uniform", "gauss"}
TORCH_GLOBAL = {"rand", "randn", "randint", "randperm", "normal", "bernoulli", "multinomial", "dropout"}
SEEDERS = {"np": ("np.random.seed", "numpy.random.seed"), "py": ("random.seed",), "torch": ("torch.manual_seed", "torch.cuda.manual_seed_all")}
BROAD_EXCEPT = {"Exception", "BaseException"}
CLEANUP = {"kill", "terminate", "close", "unlink", "remove", "rmtree", "shutdown", "wait", "cancel", "stop", "rmdir", "disconnect"}
SKIP_DIRS = {"target", "vendor", "node_modules", ".venv", "venv", "__pycache__", ".git"}
VALUE_FLAGS = ("--frozen", "--base", "--exclude", "--baseline")
ALL_FLAGS = (*VALUE_FLAGS, "--check", "--json")


@dataclass
class Finding:
    path: str
    line: int
    kind: str
    message: str
    probability: float = 0.9
    source: str = ""


def dotted(node):
    parts = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
    return ".".join(reversed(parts))


def global_family(name):
    if name.startswith(NP_PREFIXES) and name.rsplit(".", 1)[1] in NP_GLOBAL:
        return "np"
    if name.startswith("random.") and name.split(".")[1] in PY_GLOBAL:
        return "py"
    if name.startswith("torch.") and name.split(".")[1] in TORCH_GLOBAL:
        return "torch"
    return None


def is_test(path):
    p = Path(path)
    return p.name.startswith("test_") or p.name == "conftest.py" or "tests" in p.parts


def _has_seed_arg(node):
    return any(not (isinstance(a, ast.Constant) and a.value is None) for a in node.args) or any(
        not (isinstance(k.value, ast.Constant) and k.value.value is None) for k in node.keywords
    )


def lint_source(text, path="<stdin>"):
    try:
        tree = ast.parse(text)
    except SyntaxError as err:
        return [Finding(path, err.lineno or 1, "syntax", "unparseable file", 1.0)]
    direct_map = {}
    mod_aliases = {}
    seeders = {fam: set(names) for fam, names in SEEDERS.items()}
    rng_ctors = {"np.random.default_rng", "numpy.random.default_rng", "random.Random", "np.random.RandomState"}
    for n in ast.walk(tree):
        if isinstance(n, ast.Import):
            for a in n.names:
                if a.asname and a.name in ("random", "numpy.random", "torch"):
                    mod_aliases[a.asname] = a.name
        elif isinstance(n, ast.ImportFrom):
            if n.module == "random":
                for a in n.names:
                    local = a.asname or a.name
                    if a.name in PY_GLOBAL:
                        direct_map[local] = f"random.{a.name}"
                    elif a.name == "seed":
                        seeders["py"].add(local)
                    elif a.name == "Random":
                        rng_ctors.add(local)
            elif n.module == "numpy.random":
                for a in n.names:
                    local = a.asname or a.name
                    if a.name in NP_GLOBAL:
                        direct_map[local] = f"np.random.{a.name}"
                    elif a.name == "seed":
                        seeders["np"].add(local)
                    elif a.name in ("default_rng", "RandomState"):
                        rng_ctors.add(local)

    def normalize(raw):
        if raw in direct_map:
            return direct_map[raw]
        head, sep, tail = raw.partition(".")
        if sep and head in mod_aliases:
            return f"{mod_aliases[head]}.{tail}"
        return raw

    calls = [(normalize(dotted(n.func)), n) for n in ast.walk(tree) if isinstance(n, ast.Call)]
    seeded = {fam for fam, names in seeders.items() if any(name in names and _has_seed_arg(n) for name, n in calls)}
    out = []
    for name, node in calls:
        bare = not _has_seed_arg(node)
        if name in rng_ctors and bare:
            out.append(Finding(path, node.lineno, "unseeded_rng", f"{name}() has no seed"))
        family = global_family(name)
        if family and any(k.arg == "generator" for k in node.keywords):
            family = None
        if family:
            note = "seeded once in this file, but hidden global state breaks when call order changes" if family in seeded else "hidden global state with no seed in this file"
            out.append(Finding(path, node.lineno, "global_rng", f"{name}: {note}", 0.4 if family in seeded or is_test(path) else 0.85))
    for node in ast.walk(tree):
        if not isinstance(node, ast.Try):
            continue
        body_calls = [c for b in node.body for c in ast.walk(b) if isinstance(c, ast.Call)]
        cleanup = bool(body_calls) and all(dotted(c.func).rsplit(".", 1)[-1] in CLEANUP for c in body_calls)
        for h in node.handlers:
            broad = h.type is None or dotted(h.type) in BROAD_EXCEPT
            if broad and all(isinstance(b, (ast.Pass, ast.Continue)) for b in h.body):
                out.append(Finding(path, h.lineno, "swallowed_error", "broad except that does nothing hides failed runs", 0.4 if cleanup else 0.85))
    lines = text.splitlines()
    for f in out:
        f.source = lines[f.line - 1].strip() if 0 < f.line <= len(lines) else ""
    return out


def load_baseline(path):
    """Lines `path-glob | kind | source substring  # reason`. Matches by source text, so edits above a line do not break it."""
    entries = []
    for raw in Path(path).read_text().splitlines():
        body = raw.split("  #", 1)[0].strip()
        if body and not body.startswith("#"):
            glob, kind, needle = (x.strip() for x in body.split("|", 2))
            entries.append((glob, kind, needle))
    return entries


def apply_baseline(findings, entries):
    used, kept = set(), []
    for f in findings:
        hit = next((i for i, (g, k, n) in enumerate(entries) if k == f.kind and fnmatch.fnmatch(f.path, g) and n in f.source), None)
        if hit is None:
            kept.append(f)
        else:
            used.add(hit)
    stale = [Finding(g, 1, "stale_baseline", f"baseline entry matches nothing: {k} | {n}", 1.0) for i, (g, k, n) in enumerate(entries) if i not in used]
    return kept + stale


def frozen_edits(patterns, base, root):
    """Files changed against base (working tree included) that match a frozen glob."""
    res = subprocess.run(["git", "-C", str(root), "diff", "--name-only", base], capture_output=True, text=True, check=False)
    changed = res.stdout.split()
    return [Finding(c, 1, "frozen_edit", f"{c} matches a frozen pattern and changed vs {base}", 1.0)
            for c in changed if any(fnmatch.fnmatch(c, p) for p in patterns)]


def iter_python(paths, excludes=()):
    for raw in paths:
        if any(fnmatch.fnmatch(raw, e) for e in excludes):
            continue
        p = Path(raw)
        if p.is_dir():
            yield from (f for f in sorted(p.rglob("*.py"))
                        if not SKIP_DIRS & set(f.parts) and not any(fnmatch.fnmatch(str(f), e) for e in excludes))
        elif p.suffix == ".py":
            yield p


def option(args, flag):
    if flag not in args:
        return None
    i = args.index(flag)
    if i + 1 >= len(args) or args[i + 1] in ALL_FLAGS:
        raise SystemExit(f"missing value for {flag}")
    return args[i + 1]


def options(args, flag):
    out = []
    for i, a in enumerate(args):
        if a == flag:
            if i + 1 >= len(args) or args[i + 1] in ALL_FLAGS:
                raise SystemExit(f"missing value for {flag}")
            out.append(args[i + 1])
    return out


def main(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    frozen, base, excludes = option(args, "--frozen"), option(args, "--base") or "HEAD", options(args, "--exclude")
    baseline = option(args, "--baseline")
    paths = [a for i, a in enumerate(args) if not a.startswith("--") and (i == 0 or args[i - 1] not in ("--frozen", "--base", "--exclude", "--baseline"))]
    findings = []
    for f in iter_python(paths, excludes):
        findings += lint_source(f.read_text(encoding="utf-8", errors="replace"), str(f))
    if frozen:
        patterns = [ln.strip() for ln in Path(frozen).read_text().splitlines() if ln.strip() and not ln.startswith("#")]
        findings += frozen_edits(patterns, base, Path(frozen).resolve().parent)
    hits = [f for f in findings if f.probability >= THRESHOLD]
    if baseline:
        hits = apply_baseline(hits, load_baseline(baseline))
    if "--json" in args:
        print(json.dumps([asdict(f) for f in hits], indent=2))
    else:
        for f in hits:
            print(f"{f.path}:{f.line}: [{f.kind}] {f.message}")
    return 1 if hits and "--check" in args else 0


if __name__ == "__main__":
    sys.exit(main())
