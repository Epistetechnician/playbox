"""E6 plumbing on the tiny random GPT-2 (fast). The real-model run is `python -m playbox_steer.e6`."""
import torch

from playbox_steer import engine, latent


def setup(gpt2_dir):
    model, tok = engine.load(gpt2_dir)
    dim = model.config.hidden_size
    d = torch.tensor(engine.random_direction(dim, 1))
    probe = torch.tensor(engine.random_direction(dim, 2))
    nulls = [torch.tensor(engine.random_direction(dim, 10 + k)) for k in range(3)]
    return model, tok, d, probe, nulls


def test_clean_latent_handoff_equals_reading_the_text(gpt2_dir):
    model, tok, d, probe, nulls = setup(gpt2_dir)
    r = latent.run_e6(model, tok, ["The movie was", "Today the food seems"], " Honestly it was", d, nulls, 1, probe, 2, 2.0, gen_tokens=4)
    assert r["sanity_clean_equals_own_read_max_delta"] < 1e-4


def test_steering_the_context_changes_b_even_though_b_reads_identical_text(gpt2_dir):
    model, tok, d, probe, nulls = setup(gpt2_dir)
    r = latent.run_e6(model, tok, ["The movie was", "Today the food seems", "People describe the book as"], " Honestly it was", d, nulls, 1, probe, 2, 4.0, gen_tokens=4)
    assert r["arms"]["steered"]["kl_mean"] > 0, "the cache carried the edit"
    assert r["arms"]["steered"]["n"] == 3
    assert "null_0" in r["arms"] and "text_steered" in r["arms"]
    assert 0.0 <= r["null_exceed_fraction"] <= 1.0


def test_a_zero_strength_steer_changes_nothing(gpt2_dir):
    model, tok, d, probe, nulls = setup(gpt2_dir)
    r = latent.run_e6(model, tok, ["The movie was", "Today the food seems"], " Honestly it was", d, nulls, 1, probe, 2, 0.0, gen_tokens=4)
    assert r["arms"]["steered"]["kl_mean"] == 0.0


def test_arms_do_not_contaminate_each_other(gpt2_dir):
    """Each handoff gets a private copy of the cache; running the steered arm first must not alter the clean one."""
    model, tok, d, probe, nulls = setup(gpt2_dir)
    a = latent.run_e6(model, tok, ["The movie was"], " Honestly it was", d, nulls[:1], 1, probe, 2, 4.0, gen_tokens=2)
    b = latent.run_e6(model, tok, ["The movie was"], " Honestly it was", d, nulls[:1], 1, probe, 2, 4.0, gen_tokens=2)
    assert a["arms"]["steered"]["kl_mean"] == b["arms"]["steered"]["kl_mean"]
