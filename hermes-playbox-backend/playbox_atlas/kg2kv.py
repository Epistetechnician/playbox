"""KG2KV: convert repository code structure and Playbox room memory into provenance-tagged triples."""
from __future__ import annotations

import ast
import json
import os
import re
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable

SKIP_DIRS = frozenset({
    ".git", ".hg", ".svn", "__pycache__", "node_modules", "target", ".venv", "venv",
    ".mypy_cache", ".pytest_cache", ".ruff_cache", "dist", "build", ".tox",
})

# Regex fallback for non-Python files (Rust, JS/TS, Go).
_RUST_DEF = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(fn|struct|enum|trait|impl)\s+([A-Za-z_][A-Za-z0-9_]*)")
_JS_DEF = re.compile(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?(function|class)\s+([A-Za-z_$][A-Za-z0-9_$]*)")
_GO_DEF = re.compile(r"^\s*(func|type)\s+(?:\([^)]+\)\s*)?([A-Za-z_][A-Za-z0-9_]*)")

TRUST_REPO = "repo"
TRUST_REVIEWED = "reviewed"
TRUST_QUARANTINED = "quarantined"
TRUST_UNTRUSTED = "untrusted"
TRUSTED_TIERS = frozenset({TRUST_REPO, TRUST_REVIEWED})


@dataclass(frozen=True)
class Triple:
    """One AtlasKV-style `(entity, relation, value)` entry with hierarchy keys & Playbox provenance."""
    entity: str
    relation: str
    value: str
    root_key: str         # Level 0 hierarchy key (top package / domain)
    inter_key: str        # Level 1 hierarchy key (file / class)
    file: str = ""
    line: int = 0
    provenance: str = "repo"
    trust: str = TRUST_REPO
    end_line: int = 0

    @property
    def key_text(self) -> str:
        """Natural-language KG2KV key string (mirrors AtlasKV's `the <relation> of <entity>`)."""
        return f"the {self.relation} of {self.entity} in {self.inter_key}"

    @property
    def full_text(self) -> str:
        if self.file and self.line and self.end_line > self.line:
            loc = f"{self.file}:{self.line}-{self.end_line}"
        elif self.file and self.line:
            loc = f"{self.file}:{self.line}"
        else:
            loc = self.file
        prefix = f"[{loc}] " if loc else ""
        return f"{prefix}{self.entity} --{self.relation}--> {self.value}"

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: dict) -> Triple:
        return cls(**{k: v for k, v in d.items() if k in cls.__dataclass_fields__})


def _first_line(doc: str | None, limit: int = 160) -> str:
    if not doc:
        return ""
    for line in doc.strip().splitlines():
        s = line.strip()
        if s:
            return s[:limit]
    return ""


def _root_for(rel_path: str) -> str:
    parts = Path(rel_path).parts
    if len(parts) >= 3:
        return "/".join(parts[:2])
    if len(parts) == 2:
        return parts[0]
    return "<root>"


_CALL_NOISE = frozenset({
    "abs", "add", "all", "any", "append", "basename", "bool", "bytes", "chr", "clear", "close",
    "copy", "count", "debug", "dict", "dir", "dirname", "discard", "encode", "decode", "endswith",
    "enumerate", "error", "exception", "exists", "exit", "extend", "filter", "find", "float",
    "format", "frozenset", "getattr", "getcwd", "get", "group", "hasattr", "hash", "hex", "id",
    "index", "info", "insert", "int", "isdir", "isfile", "isinstance", "issubclass", "items",
    "iter", "join", "keys", "len", "list", "listdir", "lower", "lstrip", "map", "max", "min",
    "next", "object", "open", "ord", "pop", "print", "put", "range", "read", "readline", "readlines",
    "remove", "replace", "repr", "reverse", "reversed", "round", "rstrip", "search", "send",
    "set", "setattr", "setdefault", "sleep", "slice", "sort", "sorted", "split", "splitlines",
    "startswith", "stat", "str", "strip", "sub", "sum", "super", "tuple", "type", "union",
    "update", "upper", "values", "vars", "warn", "warning", "write", "zip",
})


def _extract_calls(node: ast.AST) -> list[str]:
    calls: list[str] = []
    seen: set[str] = set()
    for sub in ast.walk(node):
        if isinstance(sub, ast.Call):
            name = None
            if isinstance(sub.func, ast.Name):
                name = sub.func.id
            elif isinstance(sub.func, ast.Attribute):
                name = sub.func.attr
            if name and len(name) >= 3 and name not in seen and name not in _CALL_NOISE and not name.startswith("__"):
                seen.add(name)
                calls.append(name)
                if len(calls) >= 24:
                    break
    return calls


def _extract_python(path: Path, rel: str, provenance: str) -> list[Triple]:
    try:
        src = path.read_text(encoding="utf-8", errors="ignore")
        tree = ast.parse(src, filename=rel)
    except (OSError, SyntaxError, ValueError):
        return []
    root = _root_for(rel)
    mod_name = rel[:-3].replace("/", ".")
    triples: list[Triple] = []

    mod_doc = _first_line(ast.get_docstring(tree))
    if mod_doc:
        triples.append(Triple(mod_name, "summary", mod_doc, root, rel, rel, 1, provenance, TRUST_REPO))

    # Top-level imports (bounded so huge files don't swamp the index)
    imports: list[str] = []
    for node in tree.body:
        if isinstance(node, ast.Import):
            imports.extend(a.name for a in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            imports.append(node.module)
    if imports:
        uniq = list(dict.fromkeys(imports))[:20]
        triples.append(Triple(mod_name, "imports", ", ".join(uniq), root, rel, rel, 1, provenance, TRUST_REPO))

    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            cname = f"{mod_name}.{node.name}"
            inter = f"{rel}::{node.name}"
            bases = [ast.unparse(b) for b in node.bases] if hasattr(ast, "unparse") else []
            doc = _first_line(ast.get_docstring(node))
            val = f"class {node.name}({', '.join(bases)})" + (f" — {doc}" if doc else "")
            cend = int(getattr(node, "end_lineno", 0) or 0)
            triples.append(Triple(cname, "definition", val, root, inter, rel, node.lineno, provenance, TRUST_REPO, cend))
            if bases:
                base_names = [b.rsplit(".", 1)[-1] for b in bases if b and b != "object"]
                if base_names:
                    triples.append(Triple(cname, "inherits", ", ".join(base_names), root, inter, rel, node.lineno, provenance, TRUST_REPO, cend))
            for item in node.body:
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    mname = f"{cname}.{item.name}"
                    args = [a.arg for a in item.args.args[:6]]
                    mdoc = _first_line(ast.get_docstring(item))
                    sig = f"def {item.name}({', '.join(args)})" + (f" — {mdoc}" if mdoc else "")
                    mend = int(getattr(item, "end_lineno", 0) or 0)
                    triples.append(Triple(mname, "method", sig, root, inter, rel, item.lineno, provenance, TRUST_REPO, mend))
                    calls = _extract_calls(item)
                    if calls:
                        triples.append(Triple(mname, "calls", ", ".join(calls), root, inter, rel, item.lineno, provenance, TRUST_REPO, mend))
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            fname = f"{mod_name}.{node.name}"
            args = [a.arg for a in node.args.args[:6]]
            fdoc = _first_line(ast.get_docstring(node))
            sig = f"def {node.name}({', '.join(args)})" + (f" — {fdoc}" if fdoc else "")
            fend = int(getattr(node, "end_lineno", 0) or 0)
            triples.append(Triple(fname, "function", sig, root, rel, rel, node.lineno, provenance, TRUST_REPO, fend))
            calls = _extract_calls(node)
            if calls:
                triples.append(Triple(fname, "calls", ", ".join(calls), root, rel, rel, node.lineno, provenance, TRUST_REPO, fend))
    return triples


def _extract_regex(path: Path, rel: str, pattern: re.Pattern, provenance: str) -> list[Triple]:
    try:
        lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    except OSError:
        return []
    root = _root_for(rel)
    triples: list[Triple] = []
    for lineno, line in enumerate(lines, 1):
        m = pattern.match(line)
        if m:
            kind, name = m.group(1), m.group(2)
            triples.append(Triple(
                entity=f"{rel}::{name}",
                relation=kind,
                value=line.strip()[:160],
                root_key=root,
                inter_key=rel,
                file=rel,
                line=lineno,
                provenance=provenance,
                trust=TRUST_REPO,
            ))
    return triples


def build_repo_triples(
    repo_root: str | Path,
    *,
    base_commit: str = "HEAD",
    max_files: int = 4000,
    max_file_bytes: int = 256 * 1024,
    include_tests: bool = False,
) -> list[Triple]:
    """Walk `repo_root` and emit KG2KV triples for source files (pure stdlib, fast)."""
    root_path = Path(repo_root).resolve()
    provenance = f"repo:{base_commit}"
    triples: list[Triple] = []
    seen_files = 0
    for dirpath, dirnames, filenames in os.walk(root_path):
        dirnames[:] = sorted(
            d for d in dirnames
            if d not in SKIP_DIRS and not d.startswith(".") and (include_tests or d not in ("tests", "test"))
        )
        for fname in sorted(filenames):
            if seen_files >= max_files:
                return triples
            fpath = Path(dirpath) / fname
            if fpath.is_symlink():
                continue
            try:
                if fpath.stat().st_size > max_file_bytes:
                    continue
            except OSError:
                continue
            rel = fpath.relative_to(root_path).as_posix()
            ext = fpath.suffix.lower()
            if ext == ".py":
                triples.extend(_extract_python(fpath, rel, provenance))
                seen_files += 1
            elif ext == ".rs":
                triples.extend(_extract_regex(fpath, rel, _RUST_DEF, provenance))
                seen_files += 1
            elif ext in (".js", ".ts", ".jsx", ".tsx"):
                triples.extend(_extract_regex(fpath, rel, _JS_DEF, provenance))
                seen_files += 1
            elif ext == ".go":
                triples.extend(_extract_regex(fpath, rel, _GO_DEF, provenance))
                seen_files += 1
    return triples


def load_room_memory_triples(records: Iterable[dict]) -> list[Triple]:
    """Convert Playbox room memory / Harness² experience entries (`cue / lesson / evidence`) into KG2KV triples.

    Preserves Playbox's trust tiers: only entries explicitly marked `reviewed` or `repo` pass default queries;
    peer-authored or unreviewed entries stay tagged `quarantined` / `untrusted` so they never silently poison context.
    """
    out: list[Triple] = []
    for i, rec in enumerate(records):
        if not isinstance(rec, dict):
            continue
        topic = str(rec.get("topic") or rec.get("cue") or f"entry-{i}").strip()
        body = str(rec.get("body") or rec.get("lesson") or "").strip()
        if not body:
            continue
        kind = str(rec.get("kind") or "lesson").strip()
        domain = str(rec.get("domain") or "room-memory").strip()
        author = str(rec.get("author") or "unknown").strip()
        status = str(rec.get("status") or rec.get("trust") or TRUST_QUARANTINED).strip().lower()
        trust = TRUST_REVIEWED if status in ("accepted", "reviewed", "trusted") else TRUST_QUARANTINED
        evidence = rec.get("evidence") or []
        ev_str = f" (evidence: {', '.join(str(x) for x in evidence[:3])})" if evidence else ""
        out.append(Triple(
            entity=topic[:120],
            relation=kind,
            value=(body[:240] + ev_str),
            root_key=f"memory/{domain}",
            inter_key=f"memory/{domain}/{kind}",
            file=f"room-memory:{author}",
            line=0,
            provenance=f"member:{author}",
            trust=trust,
        ))
    return out


def load_room_memory_jsonl(path: str | Path) -> list[Triple]:
    p = Path(path)
    if not p.exists():
        return []
    recs = []
    for line in p.read_text(encoding="utf-8", errors="ignore").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            recs.append(json.loads(line))
        except ValueError:
            continue
    return load_room_memory_triples(recs)


def append_verified_lesson(
    path: str | Path,
    *,
    topic: str,
    lesson: str,
    evidence: list[str] | None = None,
    author: str = "playbox-gate",
    gate_receipt_sha256: str = "",
    domain: str = "verified-fixes",
) -> dict:
    """Persist a Playbox room-memory lesson to JSONL with strict gate-backed trust gating.

    Security invariant: entries are marked `reviewed` ONLY when backed by a non-empty sealed
    `gate_receipt_sha256`; unverified observations are always written as `quarantined` so they
    cannot silently inject untrusted instructions into subsequent AtlasKV queries.
    """
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    receipt_ref = gate_receipt_sha256.strip()
    ev = list(evidence or [])
    if receipt_ref and f"receipt:{receipt_ref[:16]}" not in ev:
        ev.insert(0, f"receipt:{receipt_ref[:16]}")
    rec = {
        "cue": topic.strip()[:120],
        "lesson": lesson.strip()[:240],
        "kind": "lesson",
        "domain": domain.strip() or "verified-fixes",
        "author": author.strip() or "playbox-gate",
        "status": TRUST_REVIEWED if receipt_ref else TRUST_QUARANTINED,
        "evidence": ev[:6],
        "gate_receipt_sha256": receipt_ref,
    }
    with p.open("a", encoding="utf-8") as f:
        f.write(json.dumps(rec, sort_keys=True) + "\n")
    return rec
