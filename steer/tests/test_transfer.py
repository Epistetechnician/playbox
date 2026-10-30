import json

import pytest
import torch
from transformers import GPT2Config, GPT2LMHeadModel, GPT2TokenizerFast

from playbox_steer import engine, transfer


def _save_wide(tmp_path_factory, tok_dir, n_embd, n_layer, seed):
    torch.manual_seed(seed)
    m = GPT2LMHeadModel(GPT2Config(n_layer=n_layer, n_embd=n_embd, n_head=2, n_positions=128, vocab_size=50257))
    p = tmp_path_factory.mktemp(f"tiny-{n_embd}")
    m.save_pretrained(p, safe_serialization=True)
    GPT2TokenizerFast.from_pretrained(tok_dir).save_pretrained(p)
    return p


@pytest.fixture(scope="module")
def small(tmp_path_factory, tok_dir):
    return _save_wide(tmp_path_factory, tok_dir, 16, 4, 1)


@pytest.fixture(scope="module")
def big(tmp_path_factory, tok_dir):
    return _save_wide(tmp_path_factory, tok_dir, 48, 4, 2)


def test_fit_recovers_a_known_linear_relation_and_reports_heldout_fit():
    g = torch.Generator().manual_seed(0)
    x = torch.randn(2000, 12, generator=g)
    w = torch.randn(12, 20, generator=g)
    y = x @ w + 3.0 + 0.01 * torch.randn(2000, 20, generator=g)
    m, stats = transfer.fit_map(x, y, ridge=1e-6)
    assert stats["r2_heldout"] > 0.999 and stats["train_rows"] == 1600 and stats["heldout_rows"] == 400
    assert torch.allclose(m.w, w, atol=0.02)
    assert torch.allclose(m.apply(x[:5]), y[:5], atol=0.1)
    v = torch.randn(12, generator=g)
    d = m.direction(v)
    assert abs(float(d.norm()) - 1) < 1e-5 and transfer.cosine(d, (v @ w)) > 0.999


def test_fit_does_not_hallucinate_structure_in_unrelated_data():
    g = torch.Generator().manual_seed(1)
    x, y = torch.randn(3000, 16, generator=g), torch.randn(3000, 24, generator=g)
    _, stats = transfer.fit_map(x, y)
    assert stats["r2_heldout"] < 0.05, stats        # chance level is about zero or slightly negative
    with pytest.raises(ValueError):
        transfer.fit_map(x, y[:10])


def test_zero_direction_is_refused():
    m = transfer.LinearMap(torch.zeros(4, 5), torch.zeros(4), torch.zeros(5))
    with pytest.raises(ValueError):
        m.direction(torch.ones(4))


def test_token_states_skip_the_first_token_and_match_token_counts(small):
    model, tok = engine.load(small)
    texts = ["The weather today is nice", "Once upon a time", "x"]
    s = transfer.token_states(model, tok, texts, "resid_post", 2)
    expect = sum(len(tok(t).input_ids) - 1 for t in texts if len(tok(t).input_ids) >= 2)
    assert s.shape == (expect, 16)
    long = transfer.token_states(model, tok, ["word " * 400], "resid_post", 2, max_len=20)
    assert long.shape[0] == 19


def test_end_to_end_on_two_tiny_models_is_deterministic_and_shardable(small, big, tmp_path):
    kw = dict(
        site="resid_post", small_layer=2, big_layer=2,
        positive=["I love this", "so happy today"], negative=["I hate this", "so sad today"],
        fit_texts=[f"This is sentence number {i} about the weather and the food." for i in range(40)],
        eval_prompts=["The weather today is", "I think that", "Once upon a time"], coef=0.5, nulls=2, tokens=5,
    )
    full = transfer.run_e3(small, big, **kw)
    again = transfer.run_e3(small, big, **kw)
    assert json.dumps(full, sort_keys=True) == json.dumps(again, sort_keys=True)
    assert set(full["arms"]) == {"clean", "direct", "mapped", "mapped_null0", "random_null0", "mapped_null1", "random_null1"}
    assert full["info"]["small_dim"] == 16 and full["info"]["big_dim"] == 48
    assert abs(full["info"]["cosine_mapped_vs_direct"]) <= 1 and len(full["info"]["cosine_mapped_null_vs_direct"]) == 2
    assert full["arms"]["clean"]["kl"] == 0 and full["arms"]["direct"]["kl"] > 0
    # Two shards cover every arm exactly once besides `clean`, which each recomputes identically.
    s0 = transfer.run_e3(small, big, shard=(0, 2), **kw)
    s1 = transfer.run_e3(small, big, shard=(1, 2), **kw)
    got = (set(s0["arms"]) | set(s1["arms"]))
    assert got == set(full["arms"]) and not ((set(s0["arms"]) & set(s1["arms"])) - {"clean"})
    assert s0["arms"]["clean"] == s1["arms"]["clean"] == full["arms"]["clean"]
    for n in full["arms"]:
        src = s0 if n in s0["arms"] else s1
        assert src["arms"][n] == full["arms"][n], n


def test_different_tokenizers_are_refused(small, tmp_path, tok_dir):
    from transformers import AutoTokenizer
    odd = tmp_path / "odd"
    odd.mkdir()
    for f in small.iterdir():
        if f.is_file():
            (odd / f.name).write_bytes(f.read_bytes())
    tk = AutoTokenizer.from_pretrained(tok_dir)
    tk.add_tokens(["<extra-token-for-test>"])
    tk.save_pretrained(odd)
    with pytest.raises(ValueError, match="same tokenizer"):
        transfer.run_e3(small, odd, site="resid_post", small_layer=1, big_layer=1, positive=["a b"], negative=["c d"], fit_texts=["a b c"], eval_prompts=["x"], coef=0.1, nulls=0, tokens=2)
