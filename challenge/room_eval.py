#!/usr/bin/env python3
"""The gate as a room host's evaluator (docs/host-less-rooms.md, M5).

`playbox up --room-log-evaluator challenge/room_eval.py` makes this host measure candidates when the room asks. Each
host runs the check itself and signs what it measured; the room log accepts a promotion only with receipts from at
least two distinct hosts that agree, so no single gate, and no single machine, decides.

    room_eval.py PROBLEM_HASH CANDIDATE_HASH   # what the host calls: prints one JSON line, exits non-zero on refusal
    room_eval.py put FILE                      # file a candidate under its hash (the store every host evaluates from)
    room_eval.py rule PROBLEM_FILE             # the problem's hash and the console line that registers it in a room

Standard library only (Tier B also needs what `gate.py` needs). A candidate is read from the store by its hash and
re-hashed, so a host cannot be made to measure something other than what the room named. The store is
$PLAYBOX_CANDIDATES, else challenge/state/candidates; getting the file to every host (rsync, the room's git) is the
operator's job for now.

The metric is an integer in micro-units, higher is better, so every host computes the same verdict:
  Tier A: baseline_rank / rank (exact checker plus an independent second checker; a failure is a refusal, not a low score).
  Tier B: median of candidate tokens/s over the pinned baseline's, on a sealed seed this host draws.
The tier in the receipt is $PB_TIER (0 when unset) until it is read from the host's verified attestation.
"""
from __future__ import annotations

import hashlib
import json
import os
import platform
import random
import shutil
import statistics
import sys
import tempfile
from fractions import Fraction
from pathlib import Path

HERE = Path(__file__).resolve().parent
A = HERE / "tier_a_matmul"
B = HERE / "tier_b_lm"
MICRO = 1_000_000


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def store() -> Path:
    return Path(os.environ.get("PLAYBOX_CANDIDATES") or HERE / "state" / "candidates")


def problems() -> dict:
    """Known problems by the hash of their problem file."""
    out = {}
    for p in sorted(A.glob("*.json")):
        out[sha256_file(p)] = ("a", p)
    out[sha256_file(B / "problem.json")] = ("b", B / "problem.json")
    return out


def refuse(msg: str):
    print(msg, file=sys.stderr)
    sys.exit(2)


def check2_matmul(n: int, scheme: dict) -> bool:
    """An independent second checker: random exact rational matrices, scheme output against the true product."""
    sys.path.insert(0, str(A))
    import check as c1

    r = random.Random(1)
    d = n * n
    for _ in range(8):
        a = [Fraction(r.randint(-9, 9), r.randint(1, 5)) for _ in range(d)]
        b = [Fraction(r.randint(-9, 9), r.randint(1, 5)) for _ in range(d)]
        c = [sum(a[i * n + j] * b[j * n + k] for j in range(n)) for i in range(n) for k in range(n)]
        m = [sum(c1.parse_rational(x) * y for x, y in zip(t["u"], a)) * sum(c1.parse_rational(x) * y for x, y in zip(t["v"], b)) for t in scheme["products"]]
        got = [sum(c1.parse_rational(t["w"][o]) * mk for t, mk in zip(scheme["products"], m)) for o in range(d)]
        if got != c:
            return False
    return True


def env_hash(extra: str = "") -> str:
    return sha256_bytes(f"{platform.platform()}|{platform.python_version()}|{extra}".encode())


def eval_a(problem_file: Path, cand: Path) -> dict:
    sys.path.insert(0, str(A))
    import check as c1  # the public exact checker

    prob = json.loads(problem_file.read_text())
    try:
        scheme = json.loads(cand.read_text())
    except (ValueError, UnicodeDecodeError) as e:
        refuse(f"invalid candidate JSON: {e}")
    r1 = c1.check(prob, scheme)
    if r1["verdict"] != "pass":
        refuse("checker 1: " + r1.get("reason", "fail"))
    rank = r1["score"]
    if not check2_matmul(prob["n"], scheme):
        refuse("checker 2 disagrees with checker 1")
    if rank < prob.get("literature", 0) and prob["id"] == "matmul2":
        refuse("below a proven lower bound: treat as a checker bug and escalate")
    return {
        "metric": prob["baseline"] * MICRO // rank,
        "weights": sha256_file(problem_file),  # no model here: bind the problem statement
        "env": env_hash(sha256_file(A / "check.py")),
    }


def eval_b(problem_file: Path, cand: Path) -> dict:
    sys.path.insert(0, str(HERE))
    import gate  # needs what the gate needs: the model, torch, transformers

    pb = json.load(open(problem_file))
    seed = random.SystemRandom().randrange(1, 2**31)  # sealed: this host's own
    rng = random.Random(seed)
    with tempfile.TemporaryDirectory() as td:
        ref = Path(td) / "ref.json"
        base = gate.run_bench(B / "baseline/candidate.py", seed, write_ref=ref)
        if base.get("verdict") != "pass":
            refuse("baseline run failed: " + str(base.get("reason")))
        ratios = []
        for _ in range(pb["reps"]):
            order = [("cand", cand), ("base", B / "baseline/candidate.py")]
            rng.shuffle(order)
            got = {}
            for name, path in order:
                r = gate.run_bench(path, seed, ref=ref)
                if r.get("verdict") != "pass":
                    refuse(f"{name}: {r.get('reason')}")
                got[name] = r["tok_s"]
            ratios.append(got["cand"] / got["base"])
    return {
        "metric": int(statistics.median(ratios) * MICRO),
        "weights": gate.sha256_file(gate.model_dir() / "model.safetensors"),
        "env": gate.env_hash(),
    }


def evaluate(problem_hash: str, cand_hash: str) -> dict:
    known = problems()
    if problem_hash not in known:
        refuse("this host does not know that problem")
    kind, pfile = known[problem_hash]
    cand = store() / cand_hash
    if not cand.exists():
        refuse(f"candidate {cand_hash[:12]} is not in this host's store ({store()}); put it there first")
    if sha256_file(cand) != cand_hash:
        refuse("the stored file does not hash to the name it is stored under")
    out = (eval_a if kind == "a" else eval_b)(pfile, cand)
    if out["metric"] <= 0:
        refuse("no positive metric")
    out["tier"] = int(os.environ.get("PB_TIER", "0"))
    return out


def rule_line(problem_file: Path) -> str:
    """The console line that registers this problem in a room: j=2 hosts, exact for Tier A, 10% noise and the gate's 5% threshold for Tier B."""
    h = sha256_file(problem_file)
    kind = problems().get(h, ("?",))[0]
    if kind == "a":
        return f"roomlog-problem {h} 2 0 1 {MICRO}"
    thr = json.load(open(problem_file)).get("threshold", 1.05)
    return f"roomlog-problem {h} 2 100000 {round((thr - 1) * MICRO)} {MICRO}"


def main(argv: list[str]) -> None:
    if len(argv) == 3 and argv[1] == "put":
        f = Path(argv[2])
        h = sha256_file(f)
        store().mkdir(parents=True, exist_ok=True)
        shutil.copyfile(f, store() / h)
        print(h)
    elif len(argv) == 3 and argv[1] == "rule":
        print(rule_line(Path(argv[2])))
    elif len(argv) == 3 and len(argv[1]) == 64 and len(argv[2]) == 64:
        print(json.dumps(evaluate(argv[1], argv[2])))
    else:
        refuse(__doc__ or "usage")


if __name__ == "__main__":
    main(sys.argv)
