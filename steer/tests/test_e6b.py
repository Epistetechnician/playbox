import torch

from playbox_steer import e6b, engine


def test_lexicon_scores_generated_text():
    assert e6b.lexicon_score("it was great and wonderful", "sentiment") > 0
    assert e6b.lexicon_score("it was terrible", "sentiment") < 0
    assert e6b.lexicon_score("I felt scared and afraid", "fear") > 0
    assert e6b.lexicon_score("", "fear") == 0.0


def test_a_cell_runs_and_a_zero_strength_steer_changes_nothing(gpt2_dir):
    model, tok = engine.load(gpt2_dir)
    dim = model.config.hidden_size
    d = torch.tensor(engine.random_direction(dim, 1))
    rand = [torch.tensor(engine.random_direction(dim, 10 + k)) for k in range(2)]
    sem = {"a": torch.tensor(engine.random_direction(dim, 50)), "b": torch.tensor(engine.random_direction(dim, 51))}
    r = e6b.run_cell(model, tok, ["The movie was", "Today the food seems"], "sentiment", d, rand, sem, 1, 0.0, 3)
    assert r["n"] == 6  # 2 contexts x 3 queries
    assert r["kl_steered"] == 0.0
    assert r["trait_logit"]["effect"]["mean"] == 0.0 and r["gen"]["effect"]["mean"] == 0.0
    r2 = e6b.run_cell(model, tok, ["The movie was", "Today the food seems"], "sentiment", d, rand, sem, 1, 4.0, 3)
    assert r2["kl_steered"] > 0
    assert set(r2["trait_logit"]["semantic_effects"]) == {"a", "b"}
