"""F3 verifier tests: an honest run passes; forged, replayed, tampered and leaked variants fail.

Runs are built from synthetic measurements signed with a throwaway key through challenge/common.py, so the
negative tests are fast. One slow test re-trains a real candidate to catch a *consistent* lie that only replay can see.
"""
from __future__ import annotations

import copy
import hashlib
import json
import random
import shutil
import sys
from pathlib import Path

import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

HERE = Path(__file__).resolve().parent.parent
REPO = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import harness  # noqa: E402  (only for the rule-agreement property test)
import task  # noqa: E402
import verify as V  # noqa: E402
from common import append_receipt, canon, pubkey_hex, sha256_bytes, sha256_file  # noqa: E402

SEED, SALT = 777, bytes([1]) * 32
CAL = {"target": 2.486, "eps_hold": 0.027, "theta": 0.08, "noise_cv": 0.02, "gap_mean": 0.0, "gap_sd": 0.005, "base_wall_median": 7.0,
       "base_tokens_median": 2478080, "wall_cap_s": 21.0, "token_cap": 9912320, "oracle_dev_loss": 2.01, "fixed_step_dev_losses": [2.486], "seeds": [1, 2, 3]}
REPLAY = {"seeds": [1111, 1222, 1333], "nonce": "ab" * 8}
_HOLD = task.holdout_blocks_from_secret(SEED)


def runs(c_wall, f_wall, c_hold=2.48, c_dev=2.485, reached=True, seeds=(1, 2, 3)):
    mk = lambda w, extra: [{"seed": s, "reached": reached, "reason": "reached" if reached else "wall_cap", "wall_s": w, "batches": 500, "tokens": 1_000_000,
                            "dev_loss": 2.485, **extra} for s in seeds]
    return {"cand": mk(c_wall, {"eval_dev_loss": c_dev, "eval_holdout_loss": c_hold}), "front": mk(f_wall, {})}


def _sign(work: Path, key, body: dict):
    return append_receipt(work / "receipts.jsonl", body, key)


def make_run(tmp: Path, plan: list[dict], key=None, reveal: bool = True, name: str = "run", secret_seed: int = SEED, extra_sources=None):
    """plan items: {name, wall, front_wall, hold, src_suffix}. Verdicts come from the verifier's own rule so an honest run is self-consistent."""
    work = tmp / name
    (work / "candidates").mkdir(parents=True)
    key = key or Ed25519PrivateKey.generate()
    base = HERE / "baseline.py"
    sha_b = sha256_file(base)
    shutil.copy(base, work / "candidates" / f"{sha_b}.py")
    hold = task.holdout_blocks_from_secret(secret_seed)
    g = _sign(work, key, {"kind": "genesis", "schema": 1, "protocol_sha256": sha256_file(REPO / "docs/f3/PROTOCOL.md"), "spec_sha256": sha256_file(task.SPEC_PATH),
                          "code_sha256": {n: sha256_file(HERE / n) for n in ("task.py", "baseline.py", "harness.py")}, "git_commit": "",
                          "holdout": {"scheme": "sha256-merkle-salted/v1", "root": task.merkle_root(SALT, task.holdout_items(hold)), "blocks": 256},
                          "replay_seed_commit": sha256_bytes(canon(REPLAY)), "calibration": CAL, "baseline": {"sha256": sha_b}})
    frontier, seq = sha_b, 0
    for p in plan:
        seq += 1
        src = base.read_text() + f"\n# {p['name']}\n" + p.get("src_suffix", "")
        sha = sha256_bytes(src.encode())
        (work / "candidates" / f"{sha}.py").write_text(src)
        r = runs(p["wall"], p["front_wall"], c_hold=p.get("hold", 2.48))
        v, why, ratio, gain, caught = V.rederive(CAL, task.SPEC, r)
        _sign(work, key, {"kind": "decision", "genesis": g["id"], "seq": seq, "author": p["name"], "parent": frontier, "candidate": {"sha256": sha}, "runs": r,
                          "verdict": v, "reason": why, "ratio": None if ratio is None else round(ratio, 4), "gain": None if gain is None else round(gain, 4),
                          "dev_gate": None, "holdout_gate": None, "caught_false_promotion": caught})
        if v == "promote":
            frontier = sha
    if reveal:
        seq += 1
        _sign(work, key, {"kind": "reveal", "genesis": g["id"], "seq": seq, "holdout_seed": secret_seed, "holdout_salt": SALT.hex(), "replay": REPLAY})
    return work, key, pubkey_hex(key)


PLAN = [dict(name="a", wall=3.4, front_wall=7.0),               # promote: 51% faster
        dict(name="b", wall=3.3, front_wall=3.4),               # reject: gain below threshold
        dict(name="c", wall=1.0, front_wall=3.4, hold=2.9),     # caught false promotion: fast on dev, fails the holdout
        dict(name="d", wall=2.0, front_wall=3.4)]               # promote


def check(work, pub, mode="none", **kw):
    return V.verify(work, pub, REPO, mode, **kw)


def failed_names(rep):
    return [c["name"] for c in rep.failed]


def rewrite(work: Path, fn):
    recs = V.read_receipts(work / "receipts.jsonl")
    fn(recs)
    (work / "receipts.jsonl").write_text("".join(json.dumps(r) + "\n" for r in recs))


def resign(work: Path, key):
    """An attacker holding the host key rebuilds the chain over edited bodies (hashes, links and signatures all valid)."""
    recs = V.read_receipts(work / "receipts.jsonl")
    prev, out = "0" * 64, []
    for r in recs:
        b = {**r["body"], "prev": prev}
        rid = sha256_bytes(canon(b))
        out.append({"id": rid, "body": b, "sig": key.sign(rid.encode()).hex()})
        prev = rid
    (work / "receipts.jsonl").write_text("".join(json.dumps(r) + "\n" for r in out))


# ---- honest ---------------------------------------------------------------------------------------------------------
def test_honest_run_verifies(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN)
    rep, s = check(work, pub)
    assert not rep.failed, rep.text()
    assert (s["promotions"], s["rejections"], s["caught_false_promotions"]) == (2, 2, 1)


def test_unrevealed_run_is_not_fully_verified(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN, reveal=False)
    assert any("reveal receipt present" in n for n in failed_names(check(work, pub)[0]))
    assert not check(work, pub, allow_unrevealed=True)[0].failed


# ---- forged receipt ---------------------------------------------------------------------------------------------------
def test_forged_signature_bytes_rejected(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN)
    rewrite(work, lambda recs: recs[2].update(sig=("0" if recs[2]["sig"][0] != "0" else "1") + recs[2]["sig"][1:]))
    assert any("signature does not verify" in c["detail"] for c in check(work, pub)[0].failed)


def test_receipt_signed_by_another_key_rejected(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN)
    attacker = Ed25519PrivateKey.generate()
    resign(work, attacker)  # a perfectly consistent chain, but not under the pinned key
    rep, _ = check(work, pub)
    assert rep.failed and "pinned" in rep.failed[0]["detail"]


def test_forged_extra_promotion_appended_rejected(tmp_path):
    work, key, pub = make_run(tmp_path, PLAN, reveal=False)
    recs = V.read_receipts(work / "receipts.jsonl")
    forged = copy.deepcopy(recs[2]["body"])
    forged.update(seq=len(recs), verdict="promote", reason="promoted")
    forged.pop("prev"), forged.pop("ts"), forged.pop("host"), forged.pop("hardware")
    attacker = Ed25519PrivateKey.generate()
    append_receipt(work / "receipts.jsonl", forged, attacker)
    assert check(work, pub, allow_unrevealed=True)[0].failed


# ---- replayed receipt -------------------------------------------------------------------------------------------------
def test_duplicated_receipt_rejected(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN, reveal=False)
    lines = (work / "receipts.jsonl").read_text().splitlines()
    (work / "receipts.jsonl").write_text("\n".join(lines + [lines[1]]) + "\n")
    assert check(work, pub, allow_unrevealed=True)[0].failed


def test_decision_spliced_from_another_run_rejected(tmp_path):
    work_a, key, pub = make_run(tmp_path, PLAN[:2], reveal=False, name="a")
    work_b, _, _ = make_run(tmp_path, PLAN[:2], key=key, reveal=False, name="b", secret_seed=999)  # same key, different genesis
    foreign = V.read_receipts(work_b / "receipts.jsonl")[1]
    # naive splice: breaks the prev link
    la = (work_a / "receipts.jsonl").read_text()
    (work_a / "receipts.jsonl").write_text(la + json.dumps(foreign) + "\n")
    assert check(work_a, pub, allow_unrevealed=True)[0].failed
    # key-holder splice: re-chained and re-signed, but still bound to the other run's genesis
    work_c, _, _ = make_run(tmp_path, PLAN[:1], key=key, reveal=False, name="c")
    recs = V.read_receipts(work_c / "receipts.jsonl") + [foreign]
    (work_c / "receipts.jsonl").write_text("".join(json.dumps(r) + "\n" for r in recs))
    resign(work_c, key)
    assert any("different genesis" in c["detail"] for c in check(work_c, pub, allow_unrevealed=True)[0].failed)


def test_reordered_receipts_rejected(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN)
    rewrite(work, lambda recs: recs.__setitem__(slice(2, 4), [recs[3], recs[2]]))
    assert check(work, pub)[0].failed


# ---- tampered metric --------------------------------------------------------------------------------------------------
def test_edited_metric_breaks_body_hash(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN)
    rewrite(work, lambda recs: [r.update() or r["body"]["runs"]["cand"][0].update(wall_s=0.1) for r in recs[2:3]])
    assert any("body was edited" in c["detail"] for c in check(work, pub)[0].failed)


def test_edited_metric_rehashed_but_not_resigned_rejected(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN)

    def f(recs):
        r = recs[2]
        r["body"]["runs"]["cand"][0]["wall_s"] = 0.1
        r["id"] = sha256_bytes(canon(r["body"]))
    rewrite(work, f)
    assert check(work, pub)[0].failed


def test_key_holder_edits_metric_but_not_verdict_rejected(tmp_path):
    """Fully valid chain, but the recorded verdict no longer follows from the recorded measurements."""
    work, key, pub = make_run(tmp_path, PLAN)

    def f(recs):  # decision 2 (b) was a gain_below_threshold reject; make its candidate look 5x faster, keep the verdict
        for r in recs[1:]:
            if r["body"].get("author") == "b":
                for x in r["body"]["runs"]["cand"]:
                    x["wall_s"] = 0.5
    rewrite(work, f)
    resign(work, key)
    rep, _ = check(work, pub)
    assert any("verdict re-derived" in n for n in failed_names(rep))


def test_flipping_a_holdout_failure_to_a_pass_rejected(tmp_path):
    work, key, pub = make_run(tmp_path, PLAN)

    def f(recs):  # hide the caught false promotion by editing its holdout loss
        for r in recs[1:]:
            if r["body"].get("author") == "c":
                for x in r["body"]["runs"]["cand"]:
                    x["eval_holdout_loss"] = 2.48
    rewrite(work, f)
    resign(work, key)
    assert any("verdict re-derived" in n for n in failed_names(check(work, pub)[0]))


def test_wrong_parent_rejected(tmp_path):
    work, key, pub = make_run(tmp_path, PLAN)
    rewrite(work, lambda recs: recs[4]["body"].update(parent=recs[0]["body"]["baseline"]["sha256"]))
    resign(work, key)
    assert any("parent is the frontier" in n for n in failed_names(check(work, pub)[0]))


def test_swapped_candidate_source_rejected(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN)
    sha = V.read_receipts(work / "receipts.jsonl")[1]["body"]["candidate"]["sha256"]
    (work / "candidates" / f"{sha}.py").write_text("# not what was evaluated\n")
    assert any("archived candidate source matches" in n for n in failed_names(check(work, pub)[0]))


@pytest.mark.slow
def test_consistent_lie_caught_only_by_replay(tmp_path):
    """The host key is stolen and a candidate that is just the baseline is recorded as a 50% faster promotion.
    Chain, signatures and rule re-derivation all pass; re-running the candidate does not."""
    work, key, pub = make_run(tmp_path, [dict(name="lie", wall=3.5, front_wall=7.0)], name="lie")
    rep, _ = check(work, pub)
    assert not rep.failed, rep.text()
    # the 'candidate' source is the baseline plus a comment, so on replay it needs the baseline's ~1200 batches, not the recorded 500
    rep, _ = check(work, pub, mode="recorded")
    assert any("recorded seeds reproduce" in n for n in failed_names(rep)), rep.text()


# ---- leaked holdout ---------------------------------------------------------------------------------------------------
def _leaky_source():
    window = ",".join(map(str, _HOLD[0][:30].tolist()))
    return f"_PEEK = [{window}]\n"


def test_candidate_embedding_holdout_tokens_flagged_by_verifier(tmp_path):
    work, _, pub = make_run(tmp_path, [dict(name="peek", wall=2.0, front_wall=7.0, src_suffix=_leaky_source())])
    assert any("no archived candidate source contains the holdout" in n for n in failed_names(check(work, pub)[0]))


def test_candidate_embedding_holdout_seed_flagged(tmp_path):
    work, _, pub = make_run(tmp_path, [dict(name="seed", wall=2.0, front_wall=7.0, src_suffix=f"_S = {SEED}\n")])
    assert any("no archived candidate source contains the holdout" in n for n in failed_names(check(work, pub)[0]))


def test_reveal_before_last_decision_flagged(tmp_path):
    work, _, pub = make_run(tmp_path, PLAN)

    def f(recs):  # move the reveal ahead of the final decision
        recs.insert(len(recs) - 2, recs.pop())
    rewrite(work, f)
    assert check(work, pub)[0].failed  # fails the chain, and the "reveal is last" rule would too


def test_reveal_before_last_decision_flagged_even_with_valid_chain(tmp_path):
    work, key, pub = make_run(tmp_path, PLAN)

    def f(recs):
        r = recs.pop()
        recs.insert(len(recs) - 1, r)
        for i, x in enumerate(recs):
            if i:
                x["body"]["seq"] = i
    rewrite(work, f)
    resign(work, key)
    assert any("reveal and it is the last receipt" in n for n in failed_names(check(work, pub)[0]))


def test_reveal_that_does_not_open_the_commitment_flagged(tmp_path):
    work, key, pub = make_run(tmp_path, PLAN)
    rewrite(work, lambda recs: recs[-1]["body"].update(holdout_seed=SEED + 1))
    resign(work, key)
    assert any("open the root" in n for n in failed_names(check(work, pub)[0]))


# ---- gates and the rule ----------------------------------------------------------------------------------------------
def test_harness_gate_rejects_leaky_and_forbidden_sources():
    assert task.leak_scan(_leaky_source(), _HOLD, SEED, SALT.hex())
    assert task.leak_scan((HERE / "baseline.py").read_text(), _HOLD, SEED, SALT.hex()) is None
    assert task.static_check((HERE / "baseline.py").read_text()) is None
    assert "disallowed" in task.static_check("import os\ndef build_model(a,b): pass\ndef train(c,m): pass\n")
    assert "disallowed" in task.static_check("def build_model(a,b): pass\ndef train(c,m):\n    open('x')\n")


def test_merkle_commitment_binds_salt_and_items():
    items = task.holdout_items(_HOLD)
    r = task.merkle_root(SALT, items)
    assert r == task.merkle_root(SALT, items)
    assert r != task.merkle_root(bytes([2]) * 32, items)
    assert r != task.merkle_root(SALT, items[:-1])
    assert r != task.merkle_root(SALT, [items[1], items[0], *items[2:]])


def test_harness_and_verifier_rules_agree():
    rng = random.Random(5)
    for _ in range(300):
        n = rng.choice([1, 2, 3])
        c_reach = [rng.random() > 0.1 for _ in range(n)]
        cand = [{"seed": s, "reached": c_reach[i], "wall_s": rng.uniform(1, 8), "eval_dev_loss": CAL["target"] - rng.choice([0, 0, 0.0005, -0.01]),
                 "eval_holdout_loss": CAL["target"] + rng.choice([-0.01, 0.0, 0.02, 0.05])} for i, s in enumerate([1, 2, 3][:n])]
        front = [{"seed": s, "reached": True, "wall_s": rng.uniform(1, 8)} for s in [1, 2, 3][:n]]
        if n < 3 and all(c_reach):
            continue  # malformed on purpose in the verifier
        if not all(c_reach):
            cand = cand[: c_reach.index(False) + 1]
            front = front[: len(cand)]
        d = harness.decide(CAL, cand, front)
        v, why, ratio, gain, caught = V.rederive(CAL, task.SPEC, {"cand": cand, "front": front})
        assert (d["verdict"], d["reason"], d["caught_false_promotion"]) == (v, why, caught)


def test_committed_dry_run_verifies():
    d = REPO / "docs/f3/dry_run"
    if not (d / "receipts.jsonl").exists():
        pytest.skip("no committed dry run")
    rep, s = V.verify(d, (d / "host.pub").read_text().strip(), REPO, "none")
    assert not rep.failed, rep.text()
    assert s["promotions"] >= 3 and s["rejections"] >= 3 and s["caught_false_promotions"] >= 1


def test_a_run_whose_data_is_not_on_this_machine_fails_cleanly(tmp_path, monkeypatch):
    """The climbmix runs need their tokenizer and shards; without them the verifier says so instead of ending in a traceback."""
    work = REPO / "docs/f3/dry_run_climbmix"
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("F3_DATA_DIR", str(tmp_path / "nothing"))
    rep, _ = V.verify(work, (work / "host.pub").read_text().strip(), REPO, "none", False, False)
    assert any("data artifacts" in n for n in failed_names(rep))
    assert any("cannot read them" in str(c) for c in rep.failed)


def test_protocol_pin_allows_appended_amendments_but_not_edits(tmp_path):
    """Amendments only append (PROTOCOL section 15): a run pinned to an earlier text still verifies after one is appended,
    but an in-place edit of the pinned text does not."""
    import subprocess

    def git(*a):
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a], cwd=tmp_path, check=True, capture_output=True)

    (tmp_path / "docs/f3").mkdir(parents=True)
    pf = tmp_path / "docs/f3/PROTOCOL.md"
    pf.write_text("rules v1\n")
    git("init", "-q")
    git("add", "-A")
    git("commit", "-q", "-m", "protocol")
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp_path, capture_output=True, text=True).stdout.strip()
    b = {"protocol_sha256": hashlib.sha256(b"rules v1\n").hexdigest(), "git_commit": commit}

    def run():
        rep = V.Report()
        return V.check_protocol(rep, b, tmp_path, pf)

    assert run()
    pf.write_text("rules v1\nA9 amendment\n")
    assert run()  # appended text
    pf.write_text("rules v2\n")
    assert not run()  # edited in place
    pf.write_text("rules v1\n")
    b["git_commit"] = None
    assert run()  # unchanged file, no git needed
    pf.write_text("rules v1\nA9\n")
    assert not run()  # appended but no commit to show the pinned text came first


# ---- no-data mode and an explicit protocol file (outside-review bundle) ------------------------------------------
def _nodata_env(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("F3_DATA_DIR", str(tmp_path / "nothing"))


def test_no_data_mode_verifies_a_climbmix_run_without_its_data_and_lists_what_it_skipped(tmp_path, monkeypatch):
    _nodata_env(tmp_path, monkeypatch)
    work = REPO / "docs/f3/dry_run_climbmix"
    rep, s = V.verify(work, (work / "host.pub").read_text().strip(), REPO, "none", no_data=True)
    assert not rep.failed, rep.text()
    skipped = [c["name"] for c in rep.checks if c.get("skipped")]
    assert any("data manifest" in n for n in skipped) and any("open the root" in n for n in skipped) and any("window" in n for n in skipped)
    assert "[SKIP]" in rep.text() and s["promotions"] >= 0 and s["receipts"] > 0


def test_no_data_mode_still_catches_a_tampered_candidate_and_refuses_replay(tmp_path, monkeypatch):
    import shutil
    _nodata_env(tmp_path, monkeypatch)
    work = tmp_path / "w"
    shutil.copytree(REPO / "docs/f3/dry_run_climbmix", work)
    pub = (work / "host.pub").read_text().strip()
    f = sorted((work / "candidates").glob("*.py"))[-1]
    f.write_text(f.read_text() + "\n# edited\n")
    rep, _ = V.verify(work, pub, REPO, "none", no_data=True)
    assert any("archived candidate source matches its hash" in n for n in failed_names(rep))
    rep, _ = V.verify(REPO / "docs/f3/dry_run_climbmix", pub, REPO, "recorded", no_data=True)
    assert any("--no-data cannot be combined" in str(c) for c in rep.failed)


def test_no_data_mode_catches_a_leaked_secret(tmp_path, monkeypatch):
    import shutil
    _nodata_env(tmp_path, monkeypatch)
    work = tmp_path / "w"
    shutil.copytree(REPO / "docs/f3/dry_run_climbmix", work)
    recs = V.read_receipts(work / "receipts.jsonl")
    salt = recs[-1]["body"]["holdout_salt"]
    # a candidate containing the salt would be flagged; the archived-hash check fails too, which is fine: the leak line must be among the failures
    (work / "candidates" / "x.py").write_text(f"S = '{salt}'\n")
    rep, _ = V.verify(work, (work / "host.pub").read_text().strip(), REPO, "none", no_data=True)
    assert any("revealed seed or salt" in n for n in failed_names(rep))


def test_protocol_file_option_verifies_without_git_history(tmp_path):
    """A repo root with no .git: the pinned protocol text is supplied as a file and must hash to the genesis pin."""
    import shutil
    root = tmp_path / "root"
    shutil.copytree(REPO / "challenge/f3", root / "challenge/f3", ignore=shutil.ignore_patterns("tests", "__pycache__"))
    work = REPO / "docs/f3/dry_run"
    pub = (work / "host.pub").read_text().strip()
    pin = V.read_receipts(work / "receipts.jsonl")[0]["body"]["protocol_sha256"]
    good = tmp_path / "PROTOCOL.md"
    good.write_bytes(subprocess_show(pin))
    rep, _ = V.verify(work, pub, root, "none", protocol_file=good, strict_git=False)
    assert not rep.failed, rep.text()
    bad = tmp_path / "BAD.md"
    bad.write_text(good.read_text() + "\nedited\n")
    rep, _ = V.verify(work, pub, root, "none", protocol_file=bad, strict_git=False)
    assert any("protocol hash" in n for n in failed_names(rep))


def subprocess_show(pin: str) -> bytes:
    import subprocess
    for c in subprocess.run(["git", "log", "--format=%H", "--", "docs/f3/PROTOCOL.md"], cwd=REPO, capture_output=True, text=True).stdout.split():
        b = subprocess.run(["git", "show", f"{c}:docs/f3/PROTOCOL.md"], cwd=REPO, capture_output=True).stdout
        if V.sha(b) == pin:
            return b
    pytest.skip("pinned protocol text not in git history")
