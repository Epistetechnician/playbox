"""The gate as a room evaluator: what a host signs is what its own checker measured, and only that."""
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
EVAL = HERE / "room_eval.py"
A = HERE / "tier_a_matmul"


def run(*args, store, tier="0"):
    env = {**os.environ, "PLAYBOX_CANDIDATES": str(store), "PB_TIER": tier}
    return subprocess.run([sys.executable, str(EVAL), *args], capture_output=True, text=True, env=env)


def put(path, store):
    r = run("put", str(path), store=store)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def phash(name="problem.json"):
    sys.path.insert(0, str(HERE))
    import room_eval

    return room_eval.sha256_file(A / name)


# REQ: ROOM-17
def test_a_valid_better_scheme_gets_the_metric_baseline_over_rank(tmp_path):
    h = put(A / "candidates/strassen.json", tmp_path)
    r = run(phash(), h, store=tmp_path, tier="2")
    assert r.returncode == 0, r.stderr
    out = json.loads(r.stdout)
    assert out["metric"] == 8 * 1_000_000 // 7 and out["tier"] == 2
    assert len(out["weights"]) == 64 and len(out["env"]) == 64


# REQ: ROOM-17
def test_the_naive_scheme_measures_exactly_the_baseline(tmp_path):
    h = put(A / "candidates/naive.json", tmp_path)
    out = json.loads(run(phash(), h, store=tmp_path).stdout)
    assert out["metric"] == 1_000_000


# REQ: ROOM-17
def test_a_wrong_scheme_is_a_refusal_not_a_low_score(tmp_path):
    h = put(A / "candidates/cheat_rank6.json", tmp_path)
    r = run(phash(), h, store=tmp_path)
    assert r.returncode != 0 and r.stdout == ""
    assert "checker" in r.stderr or "lower bound" in r.stderr


# REQ: ROOM-17
def test_a_host_measures_only_what_the_room_named(tmp_path):
    h = put(A / "candidates/strassen.json", tmp_path)
    # Not in the store.
    r = run(phash(), "0" * 64, store=tmp_path)
    assert r.returncode != 0 and "not in this host's store" in r.stderr
    # In the store under a name that is not its hash: swapped content is caught.
    (tmp_path / ("a" * 64)).write_bytes((A / "candidates/naive.json").read_bytes())
    r = run(phash(), "a" * 64, store=tmp_path)
    assert r.returncode != 0 and "does not hash" in r.stderr
    # An unknown problem.
    r = run("b" * 64, h, store=tmp_path)
    assert r.returncode != 0 and "does not know that problem" in r.stderr


# REQ: ROOM-17
def test_the_rule_line_asks_for_two_hosts_and_exact_agreement_on_tier_a(tmp_path):
    r = run("rule", str(A / "problem.json"), store=tmp_path)
    assert r.stdout.split() == ["roomlog-problem", phash(), "2", "0", "1", "1000000"], r.stdout
