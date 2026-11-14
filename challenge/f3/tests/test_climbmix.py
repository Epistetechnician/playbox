"""Climbmix substrate tests on a tiny fixture: byte-level tokenizer, three small parquet shards, no downloads."""
from __future__ import annotations

import json
import math
import os
import random
import sys
import tempfile
from pathlib import Path

import numpy as np
import pytest

HERE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

_TMP = Path(tempfile.mkdtemp(prefix="f3cm_"))
_SPEC = {"id": "f3-climbmix-test", "substrate": "climbmix", "vocab": 260, "block": 32, "dev_shard": 1, "holdout_shard": 2, "train_shards": [0],
         "train_max_tokens": 60000, "dev_rows": 16, "holdout_rows": 16, "eval_every": 5, "train_seeds": [1, 2, 3], "dev_recompute_tol": 0.001,
         "leak_window": 64, "leak_stride": 16, "threads": 2, "import_whitelist": ["torch", "numpy", "math", "time", "dataclasses", "typing"],
         "baseline": {"depth": 1, "aspect_ratio": 64, "head_dim": 32, "device_batch": 8, "total_batch": 256, "plan_steps": 50},
         "calibration": {"steps": 20, "gap_seeds": [11, 12], "noise_seed": 11, "noise_reps": 2, "theta_floor": 0.05, "eps_floor": 0.05, "wall_cap_mult": 3.0, "token_cap_mult": 4.0}}
(_TMP / "spec.json").write_text(json.dumps(_SPEC))
os.environ.update(F3_SPEC=str(_TMP / "spec.json"), F3_DATA_DIR=str(_TMP / "data"), F3_HOLDOUT_DIR=str(_TMP / "holdout"), F3_DEVICE="cpu")

import climbmix_data as cd  # noqa: E402
import task_climbmix as tc  # noqa: E402

WORDS = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega".split()


def _docs(seed, n):
    r = random.Random(seed)
    return [" ".join(r.choice(WORDS) for _ in range(r.randint(60, 200))) + f" doc{seed}-{i}." for i in range(n)]


def _write(dirp: Path, idx: int, seed: int):
    import pyarrow as pa
    import pyarrow.parquet as pq
    dirp.mkdir(parents=True, exist_ok=True)
    pq.write_table(pa.table({"text": _docs(seed, 400)}), dirp / cd.shard_name(idx))


@pytest.fixture(scope="module", autouse=True)
def fixture_data():
    import tiktoken
    _write(_TMP / "data", 0, 100)
    _write(_TMP / "data", 1, 101)
    _write(_TMP / "holdout", 2, 102)
    ranks = {bytes([i]): i for i in range(256)}
    enc = tiktoken.Encoding(name="t", pat_str=r"\p{L}+|\p{N}+|\s+|[^\s\p{L}\p{N}]+", mergeable_ranks=ranks,
                            special_tokens={n: 256 + i for i, n in enumerate(cd.SPECIAL_TOKENS)})
    cd.save_tokenizer(enc, _TMP / "data/tokenizer")
    tc.prepare_tokens()


BASE = HERE / "baseline_climbmix.py"
SECRET = {"seed": 4242, "salt": "ab" * 32}


def test_spec_is_the_fixture_spec():
    assert tc.V == 260 and tc.T == 32


def test_bpb_matches_a_hand_computation():
    import torch

    class Uniform(torch.nn.Module):
        def __init__(self):
            super().__init__()
            self.p = torch.nn.Parameter(torch.zeros(1))

        def forward(self, x):
            return torch.zeros(x.shape[0], x.shape[1], tc.V) + self.p

    rows = np.load(tc.cache_dir() / "dev_rows.npy").copy()
    rows[:, 10] = 256  # BOS (zero bytes) as a target in every row
    tb = cd.load_token_bytes()
    tgt = torch.from_numpy(rows[:, 1:]).long().reshape(-1)
    mask = tb[tgt] > 0
    want = int(mask.sum()) * math.log(tc.V) / (math.log(2) * int(tb[tgt].sum()))
    assert tc.mean_bpb(Uniform(), rows, tb) == pytest.approx(want, rel=1e-5)
    assert int((tb[tgt] == 0).sum()) > 0  # BOS targets exist and are excluded


def test_train_stream_and_dev_rows_come_from_the_right_shards():
    assert (tc.cache_dir() / "train_tokens.npy").exists()
    dev = np.load(tc.cache_dir() / "dev_rows.npy")
    assert dev.shape == (16, 33) and dev.max() < 260
    train = np.load(tc.cache_dir() / "train_tokens.npy")
    assert len(train) == 60000


def test_holdout_is_deterministic_by_seed_and_from_the_custodian_shard():
    a, b, c = tc.holdout_from_secret(SECRET), tc.holdout_from_secret(SECRET), tc.holdout_from_secret({**SECRET, "seed": 9})
    assert a["rows"].shape == (16, 33) and (a["rows"] == b["rows"]).all() and not (a["rows"] == c["rows"]).all()
    assert all("doc102-" in d for d in a["docs"])  # the custodian shard, not train or dev
    salt = bytes.fromhex(SECRET["salt"])
    r = tc.merkle_root(salt, tc.holdout_items(a))
    assert r == tc.merkle_root(salt, tc.holdout_items(b)) and r != tc.merkle_root(bytes(32), tc.holdout_items(a)) and r != tc.merkle_root(salt, tc.holdout_items(c))


def test_leak_scan_and_static_check():
    H = tc.holdout_from_secret(SECRET)
    clean = BASE.read_text()
    assert tc.leak_scan(clean, H, SECRET["seed"], SECRET["salt"]) is None
    assert tc.static_check(clean) is None
    assert tc.leak_scan(clean + f'\n_X = """{H["docs"][0][:200]}"""\n', H, SECRET["seed"], SECRET["salt"])
    assert tc.leak_scan(clean + f"\n_S = {SECRET['seed']}\n", H, SECRET["seed"], SECRET["salt"])
    assert "disallowed" in tc.static_check("import os\ndef build_model(a,b,c): pass\ndef train(c,m): pass\n")


def test_calibration_steps_convert_to_micro_batches():
    assert tc.batches_per_step() == _SPEC["baseline"]["total_batch"] // (_SPEC["baseline"]["device_batch"] * _SPEC["block"]) == 1


def test_candidate_process_never_gets_the_holdout_directory():
    with tempfile.TemporaryDirectory() as s:
        assert "F3_HOLDOUT_DIR" not in tc._env(s, holdout=False)
        assert "F3_HOLDOUT_DIR" in tc._env(s, holdout=True)


def test_run_is_deterministic_and_stops_on_target_and_caps(tmp_path):
    a = tc.run_candidate(BASE, 1, None, stop_batches=10)
    b = tc.run_candidate(BASE, 1, None, stop_batches=10)
    assert a["reason"] == "stop_batches" and a["batches"] == 10 and a["dev_loss"] == b["dev_loss"] and a["wall_s"] > 0
    r = tc.run_candidate(BASE, 1, tmp_path / "w.pt", target=99.0)
    assert r["reached"] and r["batches"] == _SPEC["eval_every"] and (tmp_path / "w.pt").exists()
    assert tc.run_candidate(BASE, 1, None, target=0.001, token_cap=3000)["reason"] == "token_cap"


def test_evaluator_recomputes_dev_and_scores_the_holdout(tmp_path):
    r = tc.run_candidate(BASE, 2, tmp_path / "w.pt", target=99.0)
    e = tc.evaluate_state(BASE, tmp_path / "w.pt", tc.holdout_from_secret(SECRET))
    assert abs(e["dev_loss"] - r["dev_loss"]) < 1e-4 and math.isfinite(e["holdout_loss"])


def test_dev_memorizer_runs_through_the_gate_plumbing(tmp_path):
    """The injected-bad shape: a patch that trains on the visible dev rows passes the static check and runs; the holdout is scored separately."""
    src = BASE.read_text().replace("x, y = ctx.next_batch(hp[\"device_batch\"])",
                                   "import numpy as np\n            _i = np.random.randint(0, len(ctx.dev_blocks), hp[\"device_batch\"])\n"
                                   "            _b = torch.from_numpy(ctx.dev_blocks[_i]).long().to(ctx.device)\n            ctx.batches += 1\n"
                                   "            ctx.tokens += hp[\"device_batch\"] * ctx.block_size\n            x, y = _b[:, :-1], _b[:, 1:]")
    assert src != BASE.read_text() and tc.static_check(src) is None
    bad = tmp_path / "bad.py"
    bad.write_text(src)
    r = tc.run_candidate(bad, 1, tmp_path / "w.pt", target=99.0)
    assert r["reached"]
    e = tc.evaluate_state(bad, tmp_path / "w.pt", tc.holdout_from_secret(SECRET))
    assert abs(e["dev_loss"] - r["dev_loss"]) < 1e-4 and math.isfinite(e["holdout_loss"])


@pytest.mark.slow
def test_harness_end_to_end_on_the_fixture_substrate(tmp_path):
    """init (calibration, genesis with data manifest), two submissions, reveal; the chain verifies under the host key."""
    import subprocess
    import verify as V
    env = {**os.environ, "F3_SUBSTRATE": "climbmix"}
    work = tmp_path / "w"

    def h(*a):
        p = subprocess.run([sys.executable, str(HERE / "harness.py"), *a], env=env, capture_output=True, text=True)
        assert p.returncode == 0, p.stdout[-800:] + p.stderr[-800:]

    h("init", "--work", str(work))
    same = tmp_path / "same.py"
    same.write_text("# unchanged baseline\n" + BASE.read_text())
    h("submit", "--work", str(work), str(same), "--author", "noop")
    mem = tmp_path / "mem.py"
    mem.write_text(BASE.read_text().replace('x, y = ctx.next_batch(hp["device_batch"])',
                   'import numpy as np\n            _i = np.random.randint(0, len(ctx.dev_blocks), hp["device_batch"])\n            _b = torch.from_numpy(ctx.dev_blocks[_i]).long().to(ctx.device)\n'
                   '            ctx.batches += 1\n            ctx.tokens += hp["device_batch"] * ctx.block_size\n            x, y = _b[:, :-1], _b[:, 1:]'))
    h("submit", "--work", str(work), str(mem), "--author", "memorizer")
    h("reveal", "--work", str(work))
    recs = V.read_receipts(work / "receipts.jsonl")
    g = recs[0]["body"]
    assert g["substrate"]["task"] == "task_climbmix" and set(g["data_manifest"]) >= {"tokenizer.pkl", "train_tokens.npy", "dev_rows.npy", "holdout_shard"}
    assert "task_climbmix.py" in g["code_sha256"] and "baseline_climbmix.py" in g["code_sha256"] and "climbmix_data.py" in g["code_sha256"]
    cal = g["calibration"]
    assert cal["eval_granularity"] == _SPEC["eval_every"] / _SPEC["calibration"]["steps"] and cal["theta"] >= cal["eval_granularity"]
    assert [r["body"]["kind"] for r in recs] == ["genesis", "decision", "decision", "reveal"]
    assert recs[1]["body"]["verdict"] == "reject"  # an unchanged baseline cannot clear the gain threshold
    rep = V.Report()
    assert V.check_chain(rep, recs, (work / "host.pub").read_text().strip()), rep.text()
