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


def imports_ok(path):
    bad = []
    for node in ast.walk(ast.parse(Path(path).read_text())):
        mods = [a.name for a in node.names] if isinstance(node, ast.Import) else [node.module or ""] if isinstance(node, ast.ImportFrom) else []
        for m in mods:
            if m.split(".")[0] not in P["allowed_imports"]:
                bad.append(m)
    return bad


def load(path, model_dir):
    spec = importlib.util.spec_from_file_location("cand", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.Model(model_dir)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidate", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--seed", type=int, required=True)
    ap.add_argument("--ref")
    ap.add_argument("--write-ref")
    a = ap.parse_args()

    bad = imports_ok(a.candidate)
    if bad:
        print(json.dumps({"verdict": "fail", "reason": f"disallowed imports: {bad}"}))
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
    for pr in prompts:
        t = time.perf_counter()
        o = model.generate(pr, n)
        total += time.perf_counter() - t
        outs.append(o)
    if a.write_ref:
        json.dump(outs, open(a.write_ref, "w"))
    res = {"tok_s": len(prompts) * n / total, "seconds": total}
    if a.ref:
        ref = json.load(open(a.ref))
        shape_ok = all(isinstance(o, list) and len(o) == n and all(isinstance(x, int) for x in o) for o in outs)
        mism = sum(1 for o, r in zip(outs, ref) if o != r)
        res.update(verdict="pass" if shape_ok and mism == 0 else "fail", mismatched_prompts=mism,
                   reason=None if shape_ok and mism == 0 else ("bad output shape" if not shape_ok else f"{mism}/{len(ref)} prompts differ from reference"))
    else:
        res["verdict"] = "pass"
    print(json.dumps(res))


if __name__ == "__main__":
    main()
