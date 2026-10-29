"""The batched path must agree with the one-at-a-time path (same device, same dtype)."""
import numpy as np
import pytest
import torch

from playbox_steer import e6b, engine, fast


@pytest.fixture(scope="module")
def parts(gpt2_dir):
    model, tok = fast.load_fast(gpt2_dir)
    dim = model.config.hidden_size
    d = torch.tensor(engine.random_direction(dim, 1))
    rand = [torch.tensor(engine.random_direction(dim, 10 + k)) for k in range(2)]
    sem = {"a": torch.tensor(engine.random_direction(dim, 50)), "b": torch.tensor(engine.random_direction(dim, 51))}
    return model, tok, d, rand, sem


CTX = ["The movie was", "Today the food seems", "Everyone says the hotel is", "After a week, the phone felt"]


def test_calibration_matches_the_slow_path(parts):
    model, tok, *_ = parts
    from types import SimpleNamespace
    from playbox_steer import latent
    slow = engine.calibrate(model, tok, latent._spec(1, 1.0, "prompt"), SimpleNamespace(prompts=tuple(CTX)))[1]
    assert fast.calibrate_fast(model, tok, CTX, 1) == pytest.approx(slow, rel=1e-4)


def test_fast_cell_equals_slow_cell(parts):
    model, tok, d, rand, sem = parts
    slow = e6b.run_cell(model, tok, CTX, "sentiment", d, rand, sem, 1, 2.0, 5)
    quick = fast.run_cell_fast(model, tok, CTX, "sentiment", d, rand, sem, 1, 2.0, 5)
    for metric in ("trait_logit",):
        assert quick[metric]["effect"]["mean"] == pytest.approx(slow[metric]["effect"]["mean"], abs=1e-3)
        assert quick[metric]["specific_vs_random"]["mean"] == pytest.approx(slow[metric]["specific_vs_random"]["mean"], abs=1e-3)
        for k, v in slow[metric]["semantic_effects"].items():
            assert quick[metric]["semantic_effects"][k] == pytest.approx(v, abs=1e-3)
    assert quick["kl_steered"] == pytest.approx(slow["kl_steered"], abs=1e-3)
    assert quick["gen"]["effect"]["mean"] == pytest.approx(slow["gen"]["effect"]["mean"], abs=0.02)
    assert quick["n"] == slow["n"] == len(CTX) * 3


def test_chunking_does_not_change_the_answer(parts):
    model, tok, d, rand, sem = parts
    a = fast.run_cell_fast(model, tok, CTX, "fear", d, rand, sem, 1, 2.0, 3, max_rows=1000)
    b = fast.run_cell_fast(model, tok, CTX, "fear", d, rand, sem, 1, 2.0, 3, max_rows=3)
    assert a["trait_logit"]["effect"]["mean"] == pytest.approx(b["trait_logit"]["effect"]["mean"], abs=1e-3)
    assert a["kl_steered"] == pytest.approx(b["kl_steered"], abs=1e-3)


def test_a_zero_strength_steer_changes_nothing(parts):
    model, tok, d, rand, sem = parts
    r = fast.run_cell_fast(model, tok, CTX, "sentiment", d, rand, sem, 1, 0.0, 3)
    assert r["kl_steered"] == pytest.approx(0.0, abs=1e-6) and r["trait_logit"]["effect"]["mean"] == pytest.approx(0.0, abs=1e-5)


def test_llama_style_model_with_native_cache_also_agrees(llama_dir):
    """SmolLM2 and Qwen use this cache implementation (updated in place) and grouped-query attention."""
    model, tok = fast.load_fast(llama_dir)
    dim = model.config.hidden_size
    d = torch.tensor(engine.random_direction(dim, 1))
    rand = [torch.tensor(engine.random_direction(dim, 10 + k)) for k in range(2)]
    sem = {"a": torch.tensor(engine.random_direction(dim, 50))}
    slow = e6b.run_cell(model, tok, CTX, "fear", d, rand, sem, 1, 2.0, 5)
    quick = fast.run_cell_fast(model, tok, CTX, "fear", d, rand, sem, 1, 2.0, 5)
    assert quick["trait_logit"]["effect"]["mean"] == pytest.approx(slow["trait_logit"]["effect"]["mean"], abs=1e-3)
    assert quick["trait_logit"]["semantic_effects"]["a"] == pytest.approx(slow["trait_logit"]["semantic_effects"]["a"], abs=1e-3)
    assert quick["kl_steered"] == pytest.approx(slow["kl_steered"], abs=1e-3)
    assert quick["gen"]["effect"]["mean"] == pytest.approx(slow["gen"]["effect"]["mean"], abs=0.02)
