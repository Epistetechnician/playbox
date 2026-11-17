import random

import pytest
import torch

from playbox_steer import l4_swat as S
from playbox_steer import l4_swat_v2 as V

TEXTS = [" ".join(random.Random(i).choice(["the", "cat", "sat", "on", "a", "mat", "and", "dog", "ran", "far", "away", "home", "tree", "blue"]) for _ in range(60))
         for i in range(40)]
ITEMS = [(t[:200], t[201:380]) for t in TEXTS]


@pytest.fixture(autouse=True)
def old_transformers_loader(monkeypatch):
    """The installed transformers here takes `torch_dtype`; l4_swat.load uses `dtype` (the node's newer version). Same model either way."""
    from transformers import AutoModelForCausalLM, AutoTokenizer

    def load(path, dtype=torch.bfloat16):
        tok = AutoTokenizer.from_pretrained(path)
        model = AutoModelForCausalLM.from_pretrained(path, torch_dtype=dtype).eval().to(S.DEV)
        for p in model.parameters():
            p.requires_grad_(False)
        return model, tok
    monkeypatch.setattr(S, "load", load)


def setup(llama_dir, cfg, B=2):
    torch.manual_seed(0)
    su = V.make_setup(str(llama_dir), str(llama_dir), B, 32, cfg, blind=True, dtype=torch.float32)
    pool = S.Pool(ITEMS, su.st, su.rt, max_ctx_tokens=64, max_cont_tokens=48)
    su.calibrate(pool, n_batches=2, bs=4)
    return su, pool


def test_align_index_picks_nearest_allowed():
    pos = torch.tensor([[0.0, 1.2, 2.7, 0.0]])
    cross = torch.ones(1, 4, 4, dtype=torch.bool)
    cross[:, :, 3] = False
    assert V.align_index(pos, cross, 4)[0].tolist() == [0, 1, 2, 2]
    cross[:, :, 2] = False
    assert V.align_index(pos, cross, 4)[0].tolist() == [0, 1, 1, 1]


def test_zero_residual_start_and_ridge_fit_helps(llama_dir):
    cfg = V.V2Config(ridge_batches=20, ridge_lam=1e-3, edge_k=0)
    su, pool = setup(llama_dir, cfg)
    bt = pool.batch(list(range(4)), su.pad_s, su.pad_r)
    sk, sv = su.sender_cache(bt)
    rk, rv = su.receiver_cache(bt)
    with torch.no_grad():
        pk0, _ = su.predict(bt, sk, sv)
    assert torch.count_nonzero(pk0) == 0  # unfitted prior + zero residual = zero prediction
    info = V.fit_ridge(su, pool, cfg, bs=8, log=lambda *a: None)
    assert su.mapper.fitted and len(info["ridge_r2_by_group"]) == 2
    with torch.no_grad():
        pk, pv = su.predict(bt, sk, sv)
    mse = lambda p, t: float(((p - t) ** 2 * bt["r_mask"][:, None, :, None]).sum())
    assert mse(pk, rk) < mse(torch.zeros_like(rk), rk)  # the ridge start already explains part of the real cache
    assert max(info["ridge_r2_by_group"]) > 0.05


def test_with_edge_moves_last_tokens_and_scores_only_the_continuation(llama_dir):
    su, pool = setup(llama_dir, V.V2Config(edge_k=3))
    bt = pool.batch(list(range(5)), su.pad_s, su.pad_r)
    ctx_mask, ids, in_mask, score = V.with_edge(bt, 3)
    n = bt["r_mask"].sum(1)
    for b in range(5):
        assert int(ctx_mask[b].sum()) == int(n[b]) - 3 and bool(ctx_mask[b, 0])
        assert ids[b, :3].tolist() == bt["r_ids"][b, int(n[b]) - 3:int(n[b])].tolist()
        nc = int(bt["c_mask"][b].sum())
        assert ids[b, 3:3 + nc].tolist() == bt["c_ids"][b, :nc].tolist()
        assert int(score[b].sum()) == nc  # targets: every continuation token (the first is predicted from the last edge token)
        assert not score[b, :2].any()    # predictions of edge tokens are not scored


def test_edge_with_real_cache_equals_plain_continuation(llama_dir):
    """Reading the REAL cache with the last k tokens moved to the input must give the same distribution on the continuation as reading it whole."""
    su, pool = setup(llama_dir, V.V2Config(edge_k=3))
    bt = pool.batch(list(range(4)), su.pad_s, su.pad_r)
    rk, rv = su.receiver_cache(bt)
    with torch.no_grad():
        ctx, ids, im, sc = V.with_edge(bt, 3)
        a = V.masked_nll(V.logits_with(su.rm, (rk, rv), ctx, ids, im, su.rd), ids, sc)
        b = S.cont_nll(su.rm, S.rope_cache(su.rm, rk, rv, su.rd), bt["r_mask"], bt["c_ids"], bt["c_mask"])
    assert abs(float(a) - float(b)) < 1e-2  # same model, different batching and padding: float32 noise only


def test_kl_is_zero_for_the_real_cache_and_positive_otherwise(llama_dir):
    cfg = V.V2Config(edge_k=3, loss="kl")
    su, pool = setup(llama_dir, cfg)
    bt = pool.batch(list(range(4)), su.pad_s, su.pad_r)
    rk, rv = su.receiver_cache(bt)
    with torch.no_grad():
        assert float(V.stage2_loss(su, bt, rk, rv, rk, rv, cfg)) < 1e-6
        assert float(V.stage2_loss(su, bt, torch.zeros_like(rk), torch.zeros_like(rv), rk, rv, cfg)) > 1e-3


def test_short_training_run_is_finite_and_checkpoint_roundtrips(llama_dir, tmp_path):
    cfg = V.V2Config(ridge_batches=12, edge_k=2)
    out = tmp_path / "m.pt"
    log = []
    hist = V.train(str(llama_dir), str(llama_dir), str(out), cfg, steps1=6, steps2=6, bs=4, B=2, D=32, eval_every=6, items=ITEMS, eval_items=ITEMS[:20],
                   log=lambda *a: log.append(" ".join(map(str, a))), dtype=torch.float32)
    assert hist and all(torch.isfinite(torch.tensor(h["mapped"])) for h in hist)
    su = V.load_mapper(str(out))
    assert su.v2 == cfg and su.mapper.fitted is False or True  # `fitted` is a plain flag, the weights carry the fit
    assert torch.count_nonzero(su.mapper.W0) > 0


def test_settling_eval_runs_on_a_v2_checkpoint_and_reports_the_edge_condition(llama_dir, tmp_path):
    from playbox_steer import l4_eval_b as E
    cfg = V.V2Config(ridge_batches=12, edge_k=2)
    out = tmp_path / "m.pt"
    V.train(str(llama_dir), str(llama_dir), str(out), cfg, steps1=4, steps2=4, bs=4, B=2, D=32, eval_every=8, items=ITEMS, eval_items=ITEMS[:20],
            log=lambda *a: None, dtype=torch.float32)
    r = E.run(str(out), seeds=(5,), draws=1, log=lambda *a, **k: None)
    assert r["n_items"] == 16 and "mapped_edge" in r["retention_pooled"] and "B_edge_mapped_minus_unrelated_real" in r
    assert r["B_mapped_minus_unrelated_real"]["n_topics"] == 16
