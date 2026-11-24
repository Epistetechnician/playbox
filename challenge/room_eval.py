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
F3 = HERE / "f3"
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
    if (F3 / "task_spec.json").is_file():
        out[sha256_file(F3 / "task_spec.json")] = ("f3", F3 / "task_spec.json")
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


def eval_f3(problem_file: Path, cand: Path) -> dict:
    """F3 training hill-climb evaluator: hardware-independent tokens-to-target with per-host sealed holdout."""
    sys.path.insert(0, str(F3))
    import task  # type: ignore

    spec = json.loads(problem_file.read_text())
    target = float(os.environ.get("PLAYBOX_F3_TARGET", "3.0"))
    eps_hold = float(os.environ.get("PLAYBOX_F3_EPS_HOLD", "0.05"))
    seeds = [int(s) for s in os.environ.get("PLAYBOX_F3_SEEDS", "1").split(",") if s.strip()]
    token_cap = int(os.environ.get("PLAYBOX_F3_TOKEN_CAP", "4000000"))
    hold_seed = int(os.environ.get("PLAYBOX_F3_HOLDOUT_SEED") or random.SystemRandom().randrange(1, 2**31))
    hold_salt = os.environ.get("PLAYBOX_F3_HOLDOUT_SALT") or os.urandom(16).hex()

    src = cand.read_text()
    bad = task.static_check(src)
    if bad:
        refuse(f"static_check: {bad}")
    hold = task.holdout_blocks_from_secret(hold_seed)
    leak = task.leak_scan(src, hold, hold_seed, hold_salt)
    if leak:
        refuse(f"leak_scan: {leak}")

    base_path = F3 / "baseline.py"
    cache_path = store() / ".f3_base_cache.json"
    try:
        base_cache = json.loads(cache_path.read_text()) if cache_path.is_file() else {}
    except (OSError, ValueError):
        base_cache = {}
    base_sig = sha256_bytes(
        f"{sha256_file(base_path)}|{sha256_file(F3 / 'task.py')}|{sha256_file(problem_file)}|{target}|{token_cap}".encode()
    )

    ratios = []
    cache_dirty = False
    with tempfile.TemporaryDirectory() as td:
        td_path = Path(td)
        cand_py = td_path / "candidate.py"
        shutil.copyfile(cand, cand_py)
        for s in seeds:
            ck = f"{base_sig}:{s}"
            if ck in base_cache:
                base_tokens = int(base_cache[ck])
            else:
                base_r = task.run_candidate(base_path, s, None, target=target, token_cap=token_cap)
                if not base_r.get("reached"):
                    refuse(f"baseline did not reach target {target} on seed {s}: {base_r.get('reason')}")
                base_tokens = int(base_r["tokens"])
                base_cache[ck] = base_tokens
                cache_dirty = True
            state_out = td_path / f"cand_s{s}.pt"
            cand_r = task.run_candidate(cand_py, s, state_out, target=target, token_cap=token_cap)
            if not cand_r.get("reached"):
                refuse(f"target_not_reached on seed {s}: {cand_r.get('reason')}")
            ev = task.evaluate_state(cand_py, state_out, hold)
            if "error" in ev:
                refuse(f"evaluate_state failed: {ev['error']}")
            if ev["dev_loss"] > target + spec.get("dev_recompute_tol", 1e-4):
                refuse(f"dev_mismatch: {ev['dev_loss']:.4f} > {target}")
            if ev["holdout_loss"] > target + eps_hold:
                refuse(
                    f"holdout_failed: holdout={ev['holdout_loss']:.4f} > limit={target + eps_hold:.4f} "
                    f"(dev={ev['dev_loss']:.4f})"
                )
            ratios.append(base_tokens / cand_r["tokens"])
    if cache_dirty:
        try:
            cache_path.write_text(json.dumps(base_cache, indent=1) + "\n")
        except OSError:
            pass

    return {
        "metric": int(round(statistics.median(ratios) * MICRO)),
        "weights": sha256_file(problem_file),
        "env": env_hash(sha256_file(F3 / "task.py")),
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
    if kind == "a":
        out = eval_a(pfile, cand)
    elif kind == "b":
        out = eval_b(pfile, cand)
    else:
        out = eval_f3(pfile, cand)
    if out["metric"] <= 0:
        refuse("no positive metric")
    out["tier"] = int(os.environ.get("PB_TIER", "0"))
    return out


def rule_line(problem_file: Path) -> str:
    """The console line that registers this problem in a room: j=2 hosts, exact for Tier A, 10% noise and the gate's 5% threshold for Tier B, 5%/5% for F3."""
    h = sha256_file(problem_file)
    kind = problems().get(h, ("?",))[0]
    if kind == "a":
        return f"roomlog-problem {h} 2 0 1 {MICRO}"
    if kind == "f3":
        return f"roomlog-problem {h} 2 50000 50000 {MICRO}"
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
