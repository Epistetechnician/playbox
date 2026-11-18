"""One timed evaluation of a candidate, in a fresh process. Prints JSON.

    python bench.py --candidate PATH --model DIR --seed S [--ref refs.json | --write-ref refs.json]

The harness owns the clock: it times only Model.generate, around each prompt, on prompts it samples itself from
the corpus with the given seed. Candidates never see the seed. Correctness is token equality with the reference.
"""
import argparse
import ast
import importlib.util
import json
import random
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
P = json.load(open(HERE / "problem.json"))


def corpus_tokens(tok):
    text = "\n".join(p.read_text() for p in sorted((ROOT / "docs").glob("*.md")))
    return tok(text)["input_ids"]


def sample_prompts(tok, seed, count):
    ids = corpus_tokens(tok)
    r = random.Random(seed)
    L = P["prompt_tokens"]
    return [ids[s:s + L] for s in (r.randrange(0, len(ids) - L) for _ in range(count))]


_DENY_NAMES = {
    "__import__", "eval", "exec", "compile", "open", "getattr", "setattr", "delattr",
    "globals", "locals", "vars", "breakpoint", "input", "__builtins__",
}
_DENY_ATTRS = {"sys", "os", "_sys", "_os"}


def imports_ok(path):
    try:
        tree = ast.parse(Path(path).read_text())
    except SyntaxError as e:
        return [f"syntax error: {e.msg}"]
    bad = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for a in node.names:
                if a.name.split(".")[0] not in P["allowed_imports"]:
                    bad.append(a.name)
        elif isinstance(node, ast.ImportFrom):
            if node.level or (node.module or "").split(".")[0] not in P["allowed_imports"]:
                bad.append(node.module or "<relative>")
        elif isinstance(node, ast.Name) and node.id in _DENY_NAMES:
            bad.append(node.id)
        elif isinstance(node, ast.Attribute):
            if node.attr in _DENY_ATTRS or (node.attr.startswith("__") and node.attr not in ("__init__", "__call__")):
                bad.append(node.attr)
    return bad


def load(path, model_dir):
    spec = importlib.util.spec_from_file_location("cand", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.Model(model_dir)


def main():
    _pc = time.perf_counter
    _stdout, _stderr = sys.stdout, sys.stderr
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidate", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--seed", type=int, required=True)
    ap.add_argument("--ref")
    ap.add_argument("--write-ref")
    a = ap.parse_args()

    bad = imports_ok(a.candidate)
    if bad:
        _stdout.write(json.dumps({"verdict": "fail", "reason": f"disallowed imports: {bad}"}) + "\n")
        return
    import torch
    from transformers import AutoTokenizer
    tok = AutoTokenizer.from_pretrained(a.model)
    prompts = sample_prompts(tok, a.seed, P["prompts"])
    warm = sample_prompts(tok, 987654, 1)[0]
    model = load(a.candidate, a.model)
    model.generate(warm, 4)  # untimed warmup on a different prompt
    n = P["new_tokens"]
    outs, total = [], 0.0
    _stderr.write("BENCH_START\n")
    _stderr.flush()
    for pr in prompts:
        t = _pc()
        o = model.generate(pr, n)
        total += _pc() - t
        outs.append(o)
    _stderr.write("BENCH_END\n")
    _stderr.flush()
    shape_ok = len(outs) == len(prompts) and all(isinstance(o, list) and len(o) == n and all(type(x) is int for x in o) for o in outs)
    if a.write_ref and shape_ok:
        json.dump(outs, open(a.write_ref, "w"))
    res = {"tok_s": (len(prompts) * n / total) if total > 0 else 0.0, "seconds": total, "outputs": outs}
    if a.ref:
        ref = json.load(open(a.ref))
        shape_ok = shape_ok and len(outs) == len(ref)
        mism = sum(1 for o, r in zip(outs, ref) if o != r) if shape_ok else len(ref)
        res.update(verdict="pass" if shape_ok and total > 0 and mism == 0 else "fail", mismatched_prompts=mism,
                   reason=None if shape_ok and total > 0 and mism == 0 else ("bad output shape" if not shape_ok else f"{mism}/{len(ref)} prompts differ from reference"))
    else:
        res.update(verdict="pass" if shape_ok and total > 0 else "fail",
                   reason=None if shape_ok and total > 0 else ("bad output shape" if not shape_ok else "non-positive elapsed time"))
    _stdout.write(json.dumps(res) + "\n")
    _stdout.flush()


if __name__ == "__main__":
    main()
