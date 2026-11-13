"""F3 harness: calibrate, evaluate candidates against the frontier, emit signed receipts, reveal the holdout.

    harness.py init   --work DIR                      # custodian secrets, calibration, genesis receipt
    harness.py submit --work DIR CAND.py --author X   # one candidate -> one signed decision receipt
    harness.py reveal --work DIR                      # custodian opens the holdout (last receipt)
    harness.py dry-run --work DIR                     # init + scripted stand-ins + reveal

Receipts go through challenge/common.py (hash-chained, Ed25519). The verifier does not import this file.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import task  # noqa: E402
from common import append_receipt, canon, pubkey_hex, sha256_bytes, sha256_file  # noqa: E402
from cryptography.hazmat.primitives import serialization  # noqa: E402
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey  # noqa: E402

SPEC = task.SPEC
REPO = HERE.parent.parent
PROTOCOL = REPO / "docs/f3/PROTOCOL.md"


def _key(work: Path) -> Ed25519PrivateKey:
    kp = work / "host.key"
    if kp.exists():
        return serialization.load_pem_private_key(kp.read_bytes(), None)
    k = Ed25519PrivateKey.generate()
    kp.write_bytes(k.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    os.chmod(kp, 0o600)
    (work / "host.pub").write_text(pubkey_hex(k) + "\n")
    return k


def _state(work: Path) -> dict:
    return json.loads((work / "state.json").read_text())


def _save(work: Path, st: dict):
    (work / "state.json").write_text(json.dumps(st, indent=1))


def _secret(work: Path) -> dict:
    return json.loads((work / "custodian/secret.json").read_text())


def _receipt(work: Path, body: dict) -> dict:
    return append_receipt(work / "receipts.jsonl", body, _key(work))


def _git(*a) -> str:
    try:
        return subprocess.run(["git", *a], cwd=REPO, capture_output=True, text=True).stdout.strip()
    except Exception:
        return ""


def _med(xs):
    return statistics.median(xs)


def _archive(work: Path, src: Path) -> str:
    sha = sha256_file(src)
    d = work / "candidates"
    d.mkdir(exist_ok=True)
    shutil.copy(src, d / f"{sha}.py")
    return sha


# ---- init: custodian, calibration, genesis -----------------------------------------------------------------
def init(work: Path, secret_seed: int | None = None, log=print) -> dict:
    work.mkdir(parents=True, exist_ok=True)
    (work / "custodian").mkdir(mode=0o700, exist_ok=True)
    sr = random.SystemRandom()
    secret = {"seed": secret_seed if secret_seed is not None else sr.randrange(10**6, 10**9), "salt": sr.randbytes(32).hex()}
    replay = {"seeds": sorted(sr.sample(range(1000, 2000), 3)), "nonce": sr.randbytes(16).hex()}
    (work / "custodian/secret.json").write_text(json.dumps({**secret, "replay": replay}))
    os.chmod(work / "custodian/secret.json", 0o600)
    hold = task.holdout_blocks_from_secret(secret["seed"])
    root = task.merkle_root(bytes.fromhex(secret["salt"]), task.holdout_items(hold))
    _key(work)

    base = _archive(work, HERE / "baseline.py")
    cal, bp = SPEC["calibration"], HERE / "baseline.py"
    seeds = SPEC["train_seeds"]
    log(f"calibration 1/3: target from {len(seeds)} baseline runs of {cal['steps']} steps")
    fixed = [task.run_candidate(bp, s, None, stop_batches=cal["steps"]) for s in seeds]
    target = math.ceil(_med([r["dev_loss"] for r in fixed]) * 1000) / 1000
    log(f"  L* = {target}")
    log("calibration 2/3: holdout gap on distinct seeds")
    gaps, walls, toks = [], [], []
    with tempfile.TemporaryDirectory() as tmp:
        for s in cal["gap_seeds"]:
            r = task.run_candidate(bp, s, Path(tmp) / "w.pt", target=target)
            assert r["reached"], r
            e = task.evaluate_state(bp, Path(tmp) / "w.pt", hold)
            gaps.append(e["holdout_loss"] - e["dev_loss"])
            walls.append(r["wall_s"])
            toks.append(r["tokens"])
    gm, gs = statistics.mean(gaps), statistics.stdev(gaps)
    eps = round(max(cal["eps_floor"], 3 * gs) + abs(gm), 4)
    log("calibration 3/3: timing noise from repeats of one seed")
    rep = [task.run_candidate(bp, cal["noise_seed"], None, target=target)["wall_s"] for _ in range(cal["noise_reps"])]
    cv = statistics.stdev(rep) / statistics.mean(rep)
    theta = max(cal["theta_floor"], math.ceil(3 * cv * 100) / 100)
    base_wall = _med(walls)
    calib = {"target": target, "eps_hold": eps, "theta": theta, "noise_cv": round(cv, 4), "gap_mean": round(gm, 5), "gap_sd": round(gs, 5),
             "base_wall_median": round(base_wall, 3), "base_tokens_median": int(_med(toks)),
             "wall_cap_s": round(cal["wall_cap_mult"] * base_wall, 3), "token_cap": int(cal["token_cap_mult"] * _med(toks)),
             "oracle_dev_loss": round(task.oracle_loss(task.gen_blocks(SPEC["dev_seed"], SPEC["dev_blocks"])), 4),
             "fixed_step_dev_losses": [round(r["dev_loss"], 4) for r in fixed], "seeds": seeds}
    log("  " + json.dumps(calib))
    body = {"kind": "genesis", "schema": 1, "protocol_sha256": sha256_file(PROTOCOL), "spec_sha256": sha256_file(task.SPEC_PATH),
            "code_sha256": {"task.py": sha256_file(HERE / "task.py"), "baseline.py": sha256_file(HERE / "baseline.py"), "harness.py": sha256_file(HERE / "harness.py")},
            "git_commit": _git("rev-parse", "HEAD"),
            "holdout": {"scheme": "sha256-merkle-salted/v1", "root": root, "blocks": SPEC["holdout_blocks"]},
            "replay_seed_commit": sha256_bytes(canon(replay)), "calibration": calib, "baseline": {"sha256": base}}
    rec = _receipt(work, body)
    _save(work, {"genesis": rec["id"], "seq": 0, "frontier": [{"sha256": base, "seq": 0}]})
    (work / "feed.jsonl").write_text("")
    log(f"genesis receipt {rec['id'][:12]}")
    return rec


# ---- decide ---------------------------------------------------------------------------------------------------
def decide(cal: dict, cand: list[dict], front: list[dict]) -> dict:
    """The preregistered promotion rule (PROTOCOL section 6) applied to measurements."""
    out = {"dev_gate": None, "holdout_gate": None, "ratio": None, "gain": None, "caught_false_promotion": False}
    if any(not r["reached"] for r in cand):
        return {**out, "verdict": "reject", "reason": "target_not_reached"}
    ratio = _med([r["wall_s"] for r in cand]) / _med([r["wall_s"] for r in front])
    out.update(ratio=round(ratio, 4), gain=round(1 - ratio, 4))
    if any(r["eval_dev_loss"] > cal["target"] + SPEC["dev_recompute_tol"] for r in cand):
        return {**out, "verdict": "reject", "reason": "dev_mismatch"}
    out["dev_gate"] = out["gain"] >= cal["theta"]
    out["holdout_gate"] = all(r["eval_holdout_loss"] <= cal["target"] + cal["eps_hold"] for r in cand)
    if not out["holdout_gate"]:
        out["caught_false_promotion"] = bool(out["dev_gate"])
        return {**out, "verdict": "reject", "reason": "holdout_failed"}
    if not out["dev_gate"]:
        return {**out, "verdict": "reject", "reason": "gain_below_threshold"}
    return {**out, "verdict": "promote", "reason": "promoted"}


# ---- submit ---------------------------------------------------------------------------------------------------
def submit(work: Path, cand: Path, author: str, log=print) -> dict:
    st = _state(work)
    genesis = next(json.loads(l) for l in (work / "receipts.jsonl").read_text().splitlines())
    cal = genesis["body"]["calibration"]
    front = st["frontier"][-1]
    front_path = work / "candidates" / f"{front['sha256']}.py"
    sha = _archive(work, cand)
    cand_path = work / "candidates" / f"{sha}.py"
    st["seq"] += 1
    sec = _secret(work)
    hold = task.holdout_blocks_from_secret(sec["seed"])
    body = {"kind": "decision", "genesis": st["genesis"], "seq": st["seq"], "author": author, "parent": front["sha256"], "candidate": {"sha256": sha}}
    src = cand_path.read_text()
    bad = task.static_check(src) or task.leak_scan(src, hold, sec["seed"], sec["salt"])
    cand_runs, front_runs = [], []
    if bad:
        body.update(runs={"cand": [], "front": []}, verdict="reject", reason="static_check", detail=bad, dev_gate=None, holdout_gate=None, ratio=None, gain=None, caught_false_promotion=False)
    else:
        with tempfile.TemporaryDirectory() as tmp:
            for s in SPEC["train_seeds"]:
                order = [("c", cand_path), ("f", front_path)]
                random.shuffle(order)
                for name, p in order:
                    if name == "c":
                        r = task.run_candidate(p, s, Path(tmp) / f"w{s}.pt", target=cal["target"], wall_cap=cal["wall_cap_s"], token_cap=cal["token_cap"])
                        if r["reached"]:
                            e = task.evaluate_state(cand_path, Path(tmp) / f"w{s}.pt", hold)
                            r.update(eval_dev_loss=e.get("dev_loss", math.inf), eval_holdout_loss=e.get("holdout_loss", math.inf))
                        cand_runs.append(r)
                    else:
                        front_runs.append(task.run_candidate(p, s, None, target=cal["target"], wall_cap=cal["wall_cap_s"] * 2, token_cap=cal["token_cap"] * 2))
                if not cand_runs[-1]["reached"]:
                    break  # a failed seed fails the candidate; do not spend the other seeds
        if len(front_runs) < len(cand_runs):
            raise RuntimeError("frontier run missing; refusing to decide")
        body["runs"] = {"cand": cand_runs, "front": front_runs}
        d = decide(cal, cand_runs, front_runs) if all(r["reached"] for r in front_runs) else {"verdict": "reject", "reason": "frontier_unmeasurable", "dev_gate": None, "holdout_gate": None, "ratio": None, "gain": None, "caught_false_promotion": False}
        body.update(d)
    rec = _receipt(work, body)
    if body["verdict"] == "promote":
        st["frontier"].append({"sha256": sha, "seq": st["seq"]})
    _save(work, st)
    redacted = {"seq": st["seq"], "author": author, "candidate": sha[:12], "verdict": body["verdict"], "reason": "holdout_failed" if body["reason"] == "holdout_failed" else body["reason"],
                "gain": body.get("gain"), "receipt": rec["id"][:12]}
    with open(work / "feed.jsonl", "a") as f:  # what agents see: no holdout loss, only the reason category
        f.write(json.dumps(redacted) + "\n")
    log(f"seq {st['seq']:2d} {author:10s} {body['verdict'].upper():8s} {body['reason']:22s} gain={body.get('gain')} caught_false={body['caught_false_promotion']}")
    return body


def reveal(work: Path, log=print) -> dict:
    st = _state(work)
    sec = _secret(work)
    st["seq"] += 1
    rec = _receipt(work, {"kind": "reveal", "genesis": st["genesis"], "seq": st["seq"], "holdout_seed": sec["seed"], "holdout_salt": sec["salt"], "replay": sec["replay"]})
    _save(work, st)
    log(f"reveal receipt {rec['id'][:12]}")
    return rec


# ---- scripted stand-ins -----------------------------------------------------------------------------------------
# Not agents. Each is a patch (old -> new text replacements) applied to the current frontier source, the same shape
# as an agent's diff. Whether a patch helps is decided by the gate, not by this table.
STAND_INS = [
    ("scripted-1", "lr_3e-3", [("re:^LR = .*$", "LR = 3e-3")]),
    ("scripted-2", "lr_3e-2", [("re:^LR = .*$", "LR = 3e-2")]),
    ("scripted-3", "warmup_50", [("re:^WARMUP = .*$", "WARMUP = 50")]),
    ("scripted-4", "wd_1e-4", [("re:^WD = .*$", "WD = 1e-4")]),
    ("scripted-5", "dev_memorizer_INJECTED_BAD", [("x, y = ctx.next_batch(BATCH)",
        "import numpy as np\n        _i = np.random.randint(0, len(ctx.dev_blocks), BATCH)\n        _b = torch.from_numpy(ctx.dev_blocks[_i]).long()\n        ctx.batches += 1\n        ctx.tokens += BATCH * ctx.block_size\n        x, y = _b[:, :-1], _b[:, 1:]")]),
    ("scripted-6", "lr_6e-3", [("re:^LR = .*$", "LR = 6e-3")]),
    ("scripted-7", "layers_1", [("re:^N_LAYER = .*$", "N_LAYER = 1")]),
    ("scripted-8", "batch_16", [("re:^BATCH = .*$", "BATCH = 16")]),
    ("scripted-9", "width_128", [("re:^D_MODEL = .*$", "D_MODEL = 128")]),
    ("scripted-10", "clip_1.0", [("re:^CLIP = .*$", "CLIP = 1.0")]),
]


def apply_patch(src: str, edits) -> str | None:
    for old, new in edits:
        if old.startswith("re:"):
            src, n = re.subn(old[3:], new, src, flags=re.M)
            if n != 1:
                return None
        elif src.count(old) != 1:
            return None
        else:
            src = src.replace(old, new)
    return src


def dry_run(work: Path, log=print):
    init(work, log=log)
    for author, name, edits in STAND_INS:
        st = _state(work)
        base_src = (work / "candidates" / f"{st['frontier'][-1]['sha256']}.py").read_text()
        new = apply_patch(base_src, edits)
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / f"{name}.py"
            if new is None:
                log(f"{author} {name}: patch does not apply to the current frontier; skipped")
                continue
            p.write_text("# proposal: " + name + "\n" + new)
            log(f"-- {author}: {name}")
            submit(work, p, author, log=log)
    reveal(work, log=log)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for k in ("init", "reveal", "dry-run", "submit"):
        s = sub.add_parser(k)
        s.add_argument("--work", required=True)
        if k == "submit":
            s.add_argument("path")
            s.add_argument("--author", required=True)
    a = ap.parse_args()
    w = Path(a.work).resolve()
    {"init": lambda: init(w), "reveal": lambda: reveal(w), "dry-run": lambda: dry_run(w), "submit": lambda: submit(w, Path(a.path), a.author)}[a.cmd]()
