"""The merge gate: evaluates a submission, writes a signed receipt, advances the frontier on a verified win.

    gate.py b CANDIDATE.py --author NAME           # Tier B: lossless speedup on the LM problem
    gate.py a SCHEME.json  --author NAME [--problem tier_a_matmul/problem.json]   # Tier A: exact checker
    gate.py credit | verify | status

Nothing here needs a person in the loop. Tier B evaluates on sealed prompt seeds that rotate after every promotion,
measures the frontier and the candidate interleaved in fresh processes, and requires a second independent seed
("replica") to agree. The replica stands in for a second host; on a real room it runs on another attested machine.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import random
import shutil
import statistics
import subprocess
import sys
import random as _r
from fractions import Fraction
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import ROOT, STATE, append_receipt, sha256_bytes, sha256_file, verify_log  # noqa: E402

B = ROOT / "tier_b_lm"
A = ROOT / "tier_a_matmul"
PB = json.load(open(B / "problem.json"))
LOG = STATE / "receipts.jsonl"
FRONT = STATE / "frontier.json"
ENV = {**os.environ, "HF_HUB_OFFLINE": "1", "TOKENIZERS_PARALLELISM": "false"}


def model_dir() -> Path:
    snaps = sorted((Path.home() / ".cache/huggingface/hub/models--HuggingFaceTB--SmolLM2-135M/snapshots").glob("*"))
    return snaps[-1]


def env_hash() -> str:
    import torch
    import transformers
    return sha256_bytes(f"{torch.__version__}|{transformers.__version__}|{sha256_file(model_dir() / 'model.safetensors')}".encode())


def load_front():
    if FRONT.exists():
        return json.loads(FRONT.read_text())
    return {"b": [{"id": "baseline", "author": "pinned", "parent": None, "path": str(B / "baseline/candidate.py"), "cum": 1.0, "ratio": 1.0}],
            "a": [{"id": "naive", "author": "pinned", "parent": None, "rank": 8}]}


def save_front(f):
    STATE.mkdir(exist_ok=True)
    FRONT.write_text(json.dumps(f, indent=1))


def run_bench(cand, seed, ref=None, write_ref=None):
    cmd = [sys.executable, str(B / "bench.py"), "--candidate", str(cand), "--model", str(model_dir()), "--seed", str(seed)]
    if ref:
        cmd += ["--ref", str(ref)]
    if write_ref:
        cmd += ["--write-ref", str(write_ref)]
    p = subprocess.run(cmd, capture_output=True, text=True, env=ENV, timeout=900)
    try:
        return json.loads(p.stdout.strip().splitlines()[-1])
    except Exception:
        return {"verdict": "fail", "reason": "harness error: " + p.stderr.strip()[-300:]}


def epoch():
    ep = STATE / "epoch.json"
    if not ep.exists():
        STATE.mkdir(exist_ok=True)
        sr = random.SystemRandom()
        e = {"primary": sr.randrange(1, 2**31), "replica": sr.randrange(1, 2**31)}
        ep.write_text(json.dumps(e))
    return json.loads(ep.read_text())


def ref_for(seed):
    p = STATE / f"ref_{seed}.json"
    if not p.exists():
        print(f"  computing reference outputs for sealed seed (baseline run)…", flush=True)
        r = run_bench(B / "baseline/candidate.py", seed, write_ref=p)
        assert r.get("verdict") == "pass", r
    return p


def submit_b(path, author):
    f = load_front()
    front = f["b"][-1]
    cand = Path(path).resolve()
    cand_hash = sha256_file(cand)
    e = epoch()
    body = {"tier": "b", "problem": PB["id"], "track": PB["track"], "problem_hash": sha256_file(B / "problem.json"),
            "env_hash": env_hash(), "candidate": cand_hash, "author": author, "parent": front["id"],
            "seed_commit": {k: sha256_bytes(str(v).encode()) for k, v in e.items()}}
    ratios, runs = {}, {}
    for role in ("primary", "replica"):
        seed = e[role]
        ref = ref_for(seed)
        cs, fs = [], []
        for rep in range(PB["reps"]):
            order = [("cand", cand), ("front", front["path"])]
            random.shuffle(order)  # randomized order each rep
            for name, pth in order:
                r = run_bench(pth, seed, ref=ref)
                if name == "cand":
                    if r.get("verdict") != "pass":
                        body.update(verdict="reject", reason=f"{role}: {r.get('reason')}", measurements=runs)
                        return finish(body, f)
                    cs.append(r["tok_s"])
                else:
                    fs.append(r["tok_s"])
        ratios[role] = statistics.median(cs) / statistics.median(fs)
        runs[role] = {"cand_tok_s": cs, "front_tok_s": fs, "ratio": round(ratios[role], 3)}
    worst = min(ratios.values())
    body["measurements"] = runs
    if worst >= PB["threshold"]:
        body.update(verdict="promote", ratio=worst)
        pid = cand_hash[:10]
        dest = STATE / "frontier"
        dest.mkdir(exist_ok=True)
        shutil.copy(cand, dest / f"{pid}.py")
        f["b"].append({"id": pid, "author": author, "parent": front["id"], "path": str(dest / f"{pid}.py"), "ratio": worst, "cum": front["cum"] * worst})
        (STATE / "epoch.json").unlink(missing_ok=True)  # seeds rotate on every promotion
    else:
        body.update(verdict="reject", reason=f"gain {worst:.3f}x over frontier is below threshold {PB['threshold']}x")
    return finish(body, f)


def check2_matmul(n, scheme):
    """Independent second checker: random exact rational matrices, compare scheme output to the true product."""
    r = _r.Random(1)
    d = n * n
    for _ in range(8):
        a = [Fraction(r.randint(-9, 9), r.randint(1, 5)) for _ in range(d)]
        b = [Fraction(r.randint(-9, 9), r.randint(1, 5)) for _ in range(d)]
        c = [sum(a[i * n + j] * b[j * n + k] for j in range(n)) for i in range(n) for k in range(n)]
        m = [sum(Fraction(x) * y for x, y in zip(t["u"], a)) * sum(Fraction(x) * y for x, y in zip(t["v"], b)) for t in scheme["products"]]
        got = [sum(Fraction(t["w"][o]) * mk for t, mk in zip(scheme["products"], m)) for o in range(d)]
        if got != c:
            return False
    return True


def submit_a(path, author, problem):
    sys.path.insert(0, str(A))
    import check as c1
    prob = json.load(open(problem))
    scheme = json.load(open(path))
    f = load_front()
    front = f["a"][-1]
    body = {"tier": "a", "problem": prob["id"], "problem_hash": sha256_file(Path(problem)), "candidate": sha256_file(Path(path)),
            "author": author, "parent": front["id"]}
    r1 = c1.check(prob, scheme)
    if r1["verdict"] != "pass":
        body.update(verdict="reject", reason="checker 1: " + r1.get("reason", ""))
    elif not check2_matmul(prob["n"], scheme):
        body.update(verdict="reject", reason="checker 2 disagrees with checker 1")
    elif r1["score"] >= front["rank"]:
        body.update(verdict="reject", reason=f"rank {r1['score']} does not improve on frontier rank {front['rank']}")
    elif r1["score"] < prob.get("literature", 0) and prob["id"] == "matmul2":
        body.update(verdict="reject", reason="below a proven lower bound: treat as a checker bug and escalate")
    else:
        body.update(verdict="promote", rank=r1["score"])
        pid = sha256_file(Path(path))[:10]
        f["a"].append({"id": pid, "author": author, "parent": front["id"], "rank": r1["score"]})
    return finish(body, f)


def finish(body, f):
    rec = append_receipt(LOG, body)
    save_front(f)
    print(f"{body['verdict'].upper():8} {body.get('reason') or ''} receipt={rec['id'][:12]}")
    return body


def credit():
    f = load_front()
    shares = {}
    for e in f["b"][1:]:
        shares.setdefault(e["author"], {"b_log_gain": 0.0, "a_rank_drop": 0})["b_log_gain"] += math.log(e["ratio"])
    prev = None
    for e in f["a"]:
        if prev is not None:
            shares.setdefault(e["author"], {"b_log_gain": 0.0, "a_rank_drop": 0})["a_rank_drop"] += prev - e["rank"]
        prev = e["rank"]
    print(json.dumps(shares, indent=1))
    print("Tier B cumulative speedup over pinned baseline:", round(f["b"][-1]["cum"], 2), "x;  Tier A rank:", f["a"][-1]["rank"])


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for k in ("a", "b"):
        s = sub.add_parser(k)
        s.add_argument("path")
        s.add_argument("--author", required=True)
        if k == "a":
            s.add_argument("--problem", default=str(A / "problem.json"))
    for k in ("credit", "verify", "status"):
        sub.add_parser(k)
    a = ap.parse_args()
    if a.cmd == "b":
        submit_b(a.path, a.author)
    elif a.cmd == "a":
        submit_a(a.path, a.author, a.problem)
    elif a.cmd == "credit":
        credit()
    elif a.cmd == "verify":
        print("receipts verified:", verify_log(LOG))
    else:
        print(json.dumps(load_front(), indent=1))
