"""Independent verifier for an F3 run. It trusts only: the pinned host public key, the committed protocol and code, and
the signed receipts. It does not import harness.py or challenge/common.py; the promotion rule is re-implemented here.

    verify.py --work RUN_DIR --host-pubkey HEX [--repo-root DIR] [--replay none|recorded|fresh|all] [--no-data] [--protocol-file FILE] [--json OUT]

Exit status 0 only if every check passes and the holdout has been revealed.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import os
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

HERE = Path(__file__).resolve().parent


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def canon(o) -> bytes:
    return json.dumps(o, sort_keys=True, separators=(",", ":")).encode()


class Report:
    def __init__(self):
        self.checks = []

    def add(self, name, ok, detail=""):
        self.checks.append({"name": name, "ok": bool(ok), "detail": detail})
        return ok

    def skip(self, name, why):
        """A check that was not run (no-data mode). Counted as neither pass nor fail; always listed in the output."""
        self.checks.append({"name": name, "ok": True, "skipped": True, "detail": why})
        return True

    @property
    def failed(self):
        return [c for c in self.checks if not c["ok"]]

    def text(self):
        return "\n".join(f"[{'SKIP' if c.get('skipped') else 'PASS' if c['ok'] else 'FAIL'}] {c['name']}" + (f": {c['detail']}" if c["detail"] else "") for c in self.checks)


# ---- 1. chain -----------------------------------------------------------------------------------------------------
def read_receipts(path: Path):
    return [json.loads(l) for l in path.read_text().splitlines() if l.strip()]


def check_chain(rep: Report, recs, host_pub: str) -> bool:
    ok = True
    if not recs:
        return rep.add("chain: receipts present", False, "empty log")
    prev, seen_ids, seen_seq = "0" * 64, set(), set()
    pub = Ed25519PublicKey.from_public_bytes(bytes.fromhex(host_pub))
    gid = recs[0]["id"]
    for i, r in enumerate(recs):
        b = r["body"]
        bad = None
        if b.get("prev") != prev:
            bad = "prev link broken"
        elif sha(canon(b)) != r["id"]:
            bad = "body hash does not match id (body was edited)"
        elif b.get("host") != host_pub:
            bad = "body names a different host key than the pinned one"
        else:
            try:
                pub.verify(bytes.fromhex(r["sig"]), r["id"].encode())
            except (InvalidSignature, ValueError):
                bad = "signature does not verify under the pinned host key"
        if not bad and r["id"] in seen_ids:
            bad = "duplicate receipt id (replay)"
        if not bad and i > 0:
            if b.get("kind") not in ("decision", "reveal"):
                bad = f"unexpected kind {b.get('kind')!r}"
            elif b.get("genesis") != gid:
                bad = "bound to a different genesis (receipt from another run)"
            elif b.get("seq") in seen_seq or b.get("seq") != i:
                bad = f"sequence number {b.get('seq')} out of order (expected {i})"
        if not bad and i == 0 and b.get("kind") != "genesis":
            bad = "first receipt is not a genesis"
        if bad:
            ok = rep.add(f"chain: receipt #{i} {r['id'][:12]}", False, bad)
            break
        seen_ids.add(r["id"])
        seen_seq.add(b.get("seq"))
        prev = r["id"]
    if ok:
        rep.add("chain: hash links, body hashes, Ed25519 signatures under the pinned key, no duplicates, sequence", True, f"{len(recs)} receipts")
    return ok


# ---- 2. genesis -----------------------------------------------------------------------------------------------------
def git(repo: Path, *a) -> str:
    try:
        p = subprocess.run(["git", *a], cwd=repo, capture_output=True, text=True)
        return p.stdout.strip() if p.returncode == 0 else ""
    except Exception:
        return ""


def check_protocol(rep: Report, b: dict, repo: Path, proto: Path) -> bool:
    """The pinned protocol hash must be the committed PROTOCOL.md. Amendments only append (section 15), so a later
    amendment may extend the file: then the text at the genesis git commit must hash to the pin and be a prefix of the
    current file. An in-place edit of the pinned text fails."""
    pin = b["protocol_sha256"]
    cur = proto.read_bytes() if proto.exists() else b""
    if cur and sha(cur) == pin:
        return rep.add("genesis: protocol hash equals the committed PROTOCOL.md", True)
    commit = b.get("git_commit")
    if cur and commit and git(repo, "cat-file", "-t", commit) == "commit":
        old = subprocess.run(["git", "show", f"{commit}:docs/f3/PROTOCOL.md"], cwd=repo, capture_output=True).stdout
        if old and sha(old) == pin and cur.startswith(old):
            return rep.add("genesis: protocol hash equals the committed PROTOCOL.md", True, f"pinned text is at git commit {commit[:10]}; later content only appends (amendments)")
    # Fallback (amendment A5): the genesis commit may not be in this repo (a run made on another host). Accept the pinned
    # hash only if it is a version of PROTOCOL.md committed here no later than the genesis receipt.
    for line in git(repo, "log", "--format=%H %ct", "--", "docs/f3/PROTOCOL.md").splitlines():
        c, ts = line.split()
        blob = subprocess.run(["git", "show", f"{c}:docs/f3/PROTOCOL.md"], cwd=repo, capture_output=True).stdout
        if blob and sha(blob) == pin and b.get("ts") is not None and int(ts) <= b["ts"]:
            return rep.add("genesis: protocol hash equals a PROTOCOL.md committed before the genesis receipt", True, f"the version at commit {c[:10]}; the file has been amended since")
    return rep.add("genesis: protocol hash equals the committed PROTOCOL.md", False, "pinned text is not an unchanged prefix of the current file, and no earlier committed version has this hash")


def check_genesis(rep: Report, g: dict, repo: Path, strict_git: bool = True, protocol_file: Path | None = None) -> bool:
    b = g["body"]
    ok = True
    proto = protocol_file or repo / "docs/f3/PROTOCOL.md"
    ok &= check_protocol(rep, b, repo, proto)
    spec = repo / "challenge/f3" / b.get("substrate", {}).get("spec", "task_spec.json")
    ok &= rep.add("genesis: task spec hash", spec.exists() and sha(spec.read_bytes()) == b["spec_sha256"], spec.name)
    commit = b.get("git_commit")
    has_commit = bool(commit and git(repo, "cat-file", "-t", commit) == "commit")
    for name, h in b["code_sha256"].items():
        f = repo / "challenge/f3" / name
        cur_ok = f.exists() and sha(f.read_bytes()) == h
        at_commit_ok = False
        if not cur_ok and has_commit:
            blob = subprocess.run(["git", "show", f"{commit}:challenge/f3/{name}"], cwd=repo, capture_output=True).stdout
            at_commit_ok = blob != b"" and sha(blob) == h
        hist_commit = None
        if not cur_ok and not at_commit_ok:
            for line in git(repo, "log", "--format=%H %ct", "--", f"challenge/f3/{name}").splitlines():
                c, ts = line.split()
                blob = subprocess.run(["git", "show", f"{c}:challenge/f3/{name}"], cwd=repo, capture_output=True).stdout
                if blob and sha(blob) == h and b.get("ts") is not None and int(ts) <= b["ts"]:
                    hist_commit = c[:10]
                    break
        detail = "" if cur_ok else (f"pinned at git commit {commit[:10]}; file updated since" if at_commit_ok else (f"the version at commit {hist_commit}; file updated since" if hist_commit else ""))
        ok &= rep.add(f"genesis: {name} hash equals the file in the repo state", cur_ok or at_commit_ok or bool(hist_commit), detail)
    first = git(repo, "log", "--diff-filter=A", "--format=%ct", "--", "docs/f3/PROTOCOL.md").splitlines()
    if first:
        ok &= rep.add("genesis: PROTOCOL.md was committed before the genesis receipt", int(first[-1]) <= b["ts"], f"protocol committed {first[-1]}, genesis ts {b['ts']}")
    if has_commit:
        for name, h in b["code_sha256"].items():
            blob = subprocess.run(["git", "show", f"{commit}:challenge/f3/{name}"], cwd=repo, capture_output=True).stdout
            match = blob != b"" and sha(blob) == h
            ok &= rep.add(f"genesis: {name} at git commit {commit[:10]} matches", match or not strict_git, "" if match else "run was started from code that is not at that commit (allowed for tests only)")
    else:
        rep.add("genesis: git commit lookup", True, "commit not present in this clone; file hashes above are the binding (soft)")
    return bool(ok)


# ---- 4. the promotion rule, re-implemented ----------------------------------------------------------------------
def rederive(cal: dict, spec: dict, runs: dict):
    """Returns (verdict, reason, ratio, gain, caught) from recorded measurements, or raises ValueError if they are malformed."""
    cand, front = runs["cand"], runs["front"]
    seeds = spec["train_seeds"]
    if [r["seed"] for r in cand] != seeds[: len(cand)] or [r["seed"] for r in front] != seeds[: len(front)]:
        raise ValueError("measurement seeds are not the preregistered seeds")
    if any(not r["reached"] for r in cand):
        return "reject", "target_not_reached", None, None, False
    if len(cand) != len(seeds) or len(front) != len(seeds) or any(not r["reached"] for r in front):
        raise ValueError("incomplete measurements for a decision that reached the target")
    ratio = statistics.median(r["wall_s"] for r in cand) / statistics.median(r["wall_s"] for r in front)
    gain = 1 - ratio
    if any(r["eval_dev_loss"] > cal["target"] + spec["dev_recompute_tol"] for r in cand):
        return "reject", "dev_mismatch", ratio, gain, False
    dev_gate = gain >= cal["theta"] - 1e-9
    hold_gate = all(r["eval_holdout_loss"] <= cal["target"] + cal["eps_hold"] for r in cand)
    if not hold_gate:
        return "reject", "holdout_failed", ratio, gain, bool(dev_gate)
    if not dev_gate:
        return "reject", "gain_below_threshold", ratio, gain, False
    return "promote", "promoted", ratio, gain, False


def check_decisions(rep: Report, recs, work: Path, spec: dict):
    g = recs[0]["body"]
    cal = g["calibration"]
    frontier = g["baseline"]["sha256"]
    promos, rejects, caught = [], [], []
    ok = rep.add("lineage: baseline archived and hashed", (work / "candidates" / f"{frontier}.py").exists() and sha((work / "candidates" / f"{frontier}.py").read_bytes()) == frontier)
    for r in recs[1:]:
        b = r["body"]
        if b["kind"] != "decision":
            continue
        tag = f"decision seq {b['seq']} ({b['candidate']['sha256'][:10]})"
        f = work / "candidates" / f"{b['candidate']['sha256']}.py"
        good = f.exists() and sha(f.read_bytes()) == b["candidate"]["sha256"]
        ok &= rep.add(f"{tag}: archived candidate source matches its hash", good)
        ok &= rep.add(f"{tag}: parent is the frontier at that point", b["parent"] == frontier, f"parent {b['parent'][:10]}, frontier {frontier[:10]}")
        if b["reason"] == "static_check":
            ok &= rep.add(f"{tag}: rejected by static check (re-checked after reveal)", True)
        else:
            try:
                v, why, ratio, gain, c = rederive(cal, spec, b["runs"])
                same = (v, why, c) == (b["verdict"], b["reason"], b["caught_false_promotion"]) and (ratio is None or abs(ratio - b["ratio"]) < 1e-3)
                ok &= rep.add(f"{tag}: verdict re-derived from recorded measurements", same,
                              f"recorded {b['verdict']}/{b['reason']} ratio={b.get('ratio')}; re-derived {v}/{why} ratio={None if ratio is None else round(ratio, 4)}")
            except (ValueError, KeyError, TypeError) as e:
                ok &= rep.add(f"{tag}: measurements well-formed", False, str(e))
        if b["verdict"] == "promote":
            promos.append(b)
            frontier = b["candidate"]["sha256"]
        else:
            rejects.append(b)
            if b.get("caught_false_promotion"):
                caught.append(b)
    return bool(ok), promos, rejects, caught


# ---- 5. holdout reveal and leak scan ----------------------------------------------------------------------------
def check_reveal(rep: Report, recs, work: Path, task, allow_unrevealed: bool, no_data: bool = False):
    g = recs[0]["body"]
    rv = [r for r in recs if r["body"]["kind"] == "reveal"]
    if not rv:
        rep.add("holdout: reveal receipt present", allow_unrevealed, "no reveal: leak scan and holdout replay cannot run" + (" (allowed)" if allow_unrevealed else ""))
        return None
    ok = rep.add("holdout: exactly one reveal and it is the last receipt", len(rv) == 1 and recs[-1]["body"]["kind"] == "reveal", "a reveal before the last decision means the holdout leaked mid-round")
    b = rv[0]["body"]
    if no_data:
        # The holdout documents are not here, so the Merkle root cannot be opened and only the secret itself can be scanned for.
        ok &= rep.skip("holdout: revealed seed and salt open the root committed in genesis", "NOT CHECKED (no-data mode: needs the holdout shard)")
        ok &= rep.add("holdout: replay seeds match the commitment in genesis", sha(canon(b["replay"])) == g["replay_seed_commit"])
        leaked = [f"{f.stem[:10]}" for f in sorted((work / "candidates").glob("*.py"))
                  if task.leak_scan(f.read_text(), {"docs": []}, b["holdout_seed"], b["holdout_salt"])]
        ok &= rep.add("holdout: no archived candidate source contains the revealed seed or salt", not leaked, ", ".join(leaked))
        rep.skip("holdout: no archived candidate source contains a window of a holdout document", "NOT CHECKED (no-data mode: needs the holdout shard)")
        return None
    hold = task.holdout_from_secret({"seed": b["holdout_seed"], "salt": b["holdout_salt"]})
    root = task.merkle_root(bytes.fromhex(b["holdout_salt"]), task.holdout_items(hold))
    ok &= rep.add("holdout: revealed seed and salt open the root committed in genesis", root == g["holdout"]["root"])
    ok &= rep.add("holdout: replay seeds match the commitment in genesis", sha(canon(b["replay"])) == g["replay_seed_commit"])
    if "data_manifest" in g and hasattr(task, "data_manifest"):
        ok &= rep.add("holdout: local holdout shard matches the data manifest", task.data_manifest(include_holdout=True).get("holdout_shard") == g["data_manifest"].get("holdout_shard"))
    leaked = []
    for f in sorted((work / "candidates").glob("*.py")):
        why = task.leak_scan(f.read_text(), hold, b["holdout_seed"], b["holdout_salt"])
        if why:
            leaked.append(f"{f.stem[:10]}: {why}")
    ok &= rep.add("holdout: no archived candidate source contains the holdout", not leaked, "; ".join(leaked))
    return hold, b


# ---- 6. replay --------------------------------------------------------------------------------------------------
def replay(rep: Report, recs, promos, caught, work: Path, task, hold, reveal, mode: str):
    cal = recs[0]["body"]["calibration"]
    spec = task.SPEC
    tol_steps = spec["eval_every"]
    for b in promos:
        sha_c, sha_p = b["candidate"]["sha256"], b["parent"]
        cp, pp = work / "candidates" / f"{sha_c}.py", work / "candidates" / f"{sha_p}.py"
        tag = f"replay seq {b['seq']} ({sha_c[:10]})"
        with tempfile.TemporaryDirectory() as tmp:
            if mode in ("recorded", "all"):
                bad = []
                for rec in b["runs"]["cand"]:
                    r = task.run_candidate(cp, rec["seed"], Path(tmp) / "w.pt", target=cal["target"], wall_cap=cal["wall_cap_s"], token_cap=cal["token_cap"])
                    if not r["reached"]:
                        bad.append(f"seed {rec['seed']}: did not reach the target on replay")
                        continue
                    e = task.evaluate_state(cp, Path(tmp) / "w.pt", hold)
                    if abs(r["batches"] - rec["batches"]) > tol_steps:
                        bad.append(f"seed {rec['seed']}: batches {r['batches']} vs recorded {rec['batches']}")
                    if abs(e["holdout_loss"] - rec["eval_holdout_loss"]) > 0.02:
                        bad.append(f"seed {rec['seed']}: holdout {e['holdout_loss']:.4f} vs recorded {rec['eval_holdout_loss']:.4f}")
                rep.add(f"{tag}: recorded seeds reproduce (batches within {tol_steps}, holdout loss within 0.02)", not bad, "; ".join(bad))
            if mode in ("fresh", "all"):
                cw, pw, bad = [], [], []
                for s in reveal["replay"]["seeds"]:
                    rc = task.run_candidate(cp, s, Path(tmp) / "w.pt", target=cal["target"], wall_cap=cal["wall_cap_s"], token_cap=cal["token_cap"])
                    rp = task.run_candidate(pp, s, None, target=cal["target"], wall_cap=cal["wall_cap_s"] * 2, token_cap=cal["token_cap"] * 2)
                    if not (rc["reached"] and rp["reached"]):
                        bad.append(f"seed {s}: target not reached")
                        continue
                    e = task.evaluate_state(cp, Path(tmp) / "w.pt", hold)
                    if e["dev_loss"] > cal["target"] + spec["dev_recompute_tol"] or e["holdout_loss"] > cal["target"] + cal["eps_hold"]:
                        bad.append(f"seed {s}: gates fail (dev {e['dev_loss']:.4f}, holdout {e['holdout_loss']:.4f})")
                    cw.append(rc["wall_s"])
                    pw.append(rp["wall_s"])
                gain = 1 - statistics.median(cw) / statistics.median(pw) if cw and not bad else None
                if gain is not None and gain < cal["theta"] / 2:
                    bad.append(f"replay gain {gain:.3f} < theta/2 = {cal['theta'] / 2:.3f}")
                rep.add(f"{tag}: fresh replay seeds {reveal['replay']['seeds']} pass gates and gain >= theta/2", not bad, "; ".join(bad) or f"gain {gain:.3f}")
    if mode != "none":
        for b in caught:
            sha_c = b["candidate"]["sha256"]
            cp = work / "candidates" / f"{sha_c}.py"
            with tempfile.TemporaryDirectory() as tmp:
                s = b["runs"]["cand"][0]["seed"]
                r = task.run_candidate(cp, s, Path(tmp) / "w.pt", target=cal["target"], wall_cap=cal["wall_cap_s"], token_cap=cal["token_cap"])
                e = task.evaluate_state(cp, Path(tmp) / "w.pt", hold) if r["reached"] else {}
                still = r["reached"] and e["holdout_loss"] > cal["target"] + cal["eps_hold"]
                rep.add(f"replay seq {b['seq']} ({sha_c[:10]}): caught false promotion fails the holdout again", still, f"holdout {e.get('holdout_loss')}, limit {cal['target'] + cal['eps_hold']:.4f}")


# ---- driver -------------------------------------------------------------------------------------------------------
def verify(work: Path, host_pub: str, repo: Path, mode: str = "none", allow_unrevealed: bool = False, strict_git: bool = True,
           no_data: bool = False, protocol_file: Path | None = None) -> tuple[Report, dict]:
    rep = Report()
    recs = read_receipts(work / "receipts.jsonl")
    summary = {}
    if not check_chain(rep, recs, host_pub):
        return rep, summary
    if not check_genesis(rep, recs[0], repo, strict_git, protocol_file):
        return rep, summary
    # The substrate module is imported only now, after its hash was checked against the genesis.
    sub = recs[0]["body"].get("substrate", {"task": "task", "spec": "task_spec.json"})
    if sub["task"] not in ("task", "task_climbmix"):
        rep.add("genesis: substrate module is a known one", False, sub["task"])
        return rep, summary
    os.environ["F3_SPEC"] = sub["spec"]
    sys.path.insert(0, str(repo / "challenge/f3"))
    task = importlib.import_module(sub["task"])
    if no_data and mode != "none":
        rep.add("replay: needs the data", False, "--no-data cannot be combined with --replay")
        return rep, summary
    if "data_manifest" in recs[0]["body"] and no_data:
        rep.skip("genesis: local data artifacts match the data manifest (tokenizer, token caches, shards)", "NOT CHECKED (no-data mode)")
    elif "data_manifest" in recs[0]["body"]:
        try:
            local = task.data_manifest(include_holdout=False)
        except OSError as e:
            rep.add("genesis: local data artifacts match the data manifest (tokenizer, token caches, shards)", False,
                    f"cannot read them: {e.filename or e}. This run's substrate needs its data on this machine (see challenge/f3/climbmix_data.py); "
                    "the receipts cannot be checked without it")
            return rep, summary
        want = recs[0]["body"]["data_manifest"]
        bad = [k for k in local if local[k] != want.get(k)]
        if not rep.add("genesis: local data artifacts match the data manifest (tokenizer, token caches, shards)", not bad, ("differs: " + ", ".join(bad)) if bad else ""):
            return rep, summary
    ok, promos, rejects, caught = check_decisions(rep, recs, work, task.SPEC)
    rv = check_reveal(rep, recs, work, task, allow_unrevealed, no_data)
    if no_data:
        pass
    elif rv and mode != "none":
        replay(rep, recs, promos, caught, work, task, rv[0], rv[1], mode)
    elif mode != "none":
        rep.add("replay: needs the reveal", False)
    summary = {"receipts": len(recs), "promotions": len(promos), "rejections": len(rejects), "caught_false_promotions": len(caught),
               "reject_reasons": {k: sum(1 for b in rejects if b["reason"] == k) for k in sorted({b["reason"] for b in rejects})}}
    return rep, summary


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", required=True)
    ap.add_argument("--host-pubkey", required=True)
    ap.add_argument("--repo-root", default=str(HERE.parent.parent))
    ap.add_argument("--replay", default="none", choices=["none", "recorded", "fresh", "all"])
    ap.add_argument("--allow-unrevealed", action="store_true")
    ap.add_argument("--allow-dirty", action="store_true", help="tests only: accept code that differs from the genesis git commit")
    ap.add_argument("--no-data", action="store_true", help="no dataset or GPU: skip the data manifest, the holdout Merkle opening and the document-window leak scan (reported as SKIP)")
    ap.add_argument("--protocol-file", help="the PROTOCOL.md text to compare with the genesis pin, instead of <repo-root>/docs/f3/PROTOCOL.md (for a bundle without git history)")
    ap.add_argument("--json")
    a = ap.parse_args()
    rep, summary = verify(Path(a.work), a.host_pubkey.strip(), Path(a.repo_root), a.replay, a.allow_unrevealed, not a.allow_dirty,
                          a.no_data, Path(a.protocol_file) if a.protocol_file else None)
    print(rep.text())
    ok = not rep.failed
    print(f"\nRESULT: {'VERIFIED' if ok else 'FAILED (' + str(len(rep.failed)) + ' checks)'}  {json.dumps(summary)}")
    if a.json:
        Path(a.json).write_text(json.dumps({"ok": ok, "summary": summary, "checks": rep.checks}, indent=1))
    sys.exit(0 if ok else 1)
