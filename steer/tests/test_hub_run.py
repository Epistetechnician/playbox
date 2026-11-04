"""E8 runner end to end on tiny random models of three architectures, with a local word-level tokenizer (no downloads).
The models are random, so this checks the plumbing and the shapes of the result, not that anything steers."""
import re

import pytest
import torch
from tokenizers import Tokenizer, models, normalizers, pre_tokenizers
from transformers import GPT2Config, GPT2LMHeadModel, LlamaConfig, LlamaForCausalLM, PreTrainedTokenizerFast

from playbox_steer import hub_run
from playbox_steer.anchors_extra import EXTRA_TOPICS, extra_anchors
from playbox_steer.telepathy_data import FIT_TOPICS, NEUTRAL, PROMPTS, TEST_CONCEPTS


def _tokenizer() -> PreTrainedTokenizerFast:
    texts = [s for t in FIT_TOPICS.values() for s in t] + [s for t in EXTRA_TOPICS.values() for s in t] + NEUTRAL + PROMPTS + [s for c in TEST_CONCEPTS.values() for s in c["sentences"]]
    words = sorted({w for t in texts for w in re.findall(r"\w+|[^\w\s]", t.lower())})
    vocab = {w: i for i, w in enumerate(["[UNK]", "[EOS]"] + words)}
    tk = Tokenizer(models.WordLevel(vocab, unk_token="[UNK]"))
    tk.normalizer = normalizers.Lowercase()
    tk.pre_tokenizer = pre_tokenizers.Whitespace()
    return PreTrainedTokenizerFast(tokenizer_object=tk, unk_token="[UNK]", eos_token="[EOS]", pad_token="[EOS]"), len(vocab)


@pytest.fixture(scope="module")
def zoo(tmp_path_factory):
    tok, v = _tokenizer()
    torch.manual_seed(0)
    nets = {
        "llama_s": LlamaForCausalLM(LlamaConfig(vocab_size=v, hidden_size=32, intermediate_size=64, num_hidden_layers=4, num_attention_heads=2, num_key_value_heads=2)),
        "llama_m": LlamaForCausalLM(LlamaConfig(vocab_size=v, hidden_size=48, intermediate_size=96, num_hidden_layers=6, num_attention_heads=4, num_key_value_heads=4)),
        "gpt2_s": GPT2LMHeadModel(GPT2Config(vocab_size=v, n_embd=40, n_layer=4, n_head=2, n_positions=128)),
    }
    out = {}
    for name, m in nets.items():
        p = tmp_path_factory.mktemp(name)
        m.save_pretrained(p, safe_serialization=True)
        tok.save_pretrained(p)
        out[name] = str(p)
    return out


def test_every_ordered_pair_runs_across_architectures(zoo):
    r = hub_run.run(zoo, k=16, coefs=(0.5,), n_null=2, gen=3, concepts=["ocean", "space"], prompts=PROMPTS[:2])
    assert set(r["pairs"]) == {f"{a}->{b}" for a in zoo for b in zoo if a != b}
    assert r["hub"]["wire_bytes_per_thought"] == 16
    assert r["hub"]["models"] == {"llama_s": 32, "llama_m": 48, "gpt2_s": 40}
    for key, cell in r["pairs"].items():
        assert set(cell["0.5"]) == {"hub", "pair", "direct", "null"}, key
        assert all(0.0 <= x <= 1.0 for x in cell["0.5"].values())
    # random models share nothing meaningful except token identity, but the held-out sentences still map better than chance
    assert all(v["hub"] > 0 for v in r["heldout_cosine"].values())
    assert r["chance"] == 0.5


def test_sweep_scores_other_sizes_and_a_joining_model(zoo):
    r = hub_run.sweep(zoo, ks=(8,), gen=2, concepts=["ocean", "space"], prompts=PROMPTS[:1])
    assert {"k8", "join_llama_s", "join_llama_m", "join_gpt2_s"} == set(r["hubs"])
    cell = r["pairs"]["llama_s->llama_m"]
    assert set(cell) == {"k8", "join_llama_s", "join_llama_m"}  # join_gpt2_s does not involve this pair
    assert all(0 <= v["acc"] <= 1 for c in r["pairs"].values() for v in c.values())


def test_anchor_sweep_fits_on_subsets_and_scores_the_full_set(zoo):
    r = hub_run.anchor_sweep(zoo, ns=(40,), ks=(8,), seeds=(0, 1), gen=2, concepts=["ocean", "space"], prompts=PROMPTS[:1])
    assert set(r["hubs"]) == {"n40_s0_k8", "n40_s1_k8", "full_k8"}
    assert all(0 <= v["acc"] <= 1 for c in r["pairs"].values() for v in c.values())
    assert set(r["pairs"]["llama_s->llama_m"]) == {"n40_s0_k8", "n40_s1_k8", "full_k8"}


def test_extra_anchors_are_clean_and_do_not_touch_the_held_out_set():
    base = {s for t in FIT_TOPICS.values() for s in t}
    more = extra_anchors(16, base)
    assert len(more) > 500 and not set(more) & base and len(set(more)) == len(more)
    lexicon = {w for c in TEST_CONCEPTS.values() for w in c["lexicon"]}
    assert not {w for s in more for w in re.findall(r"[a-z]+", s.lower())} & lexicon
    assert not set(more) & {s for c in TEST_CONCEPTS.values() for s in c["sentences"]}


def test_anchor_extend_fits_on_more_anchors(zoo):
    r = hub_run.anchor_extend(zoo, extra=(0, 2), ks=(8,), gen=2, concepts=["ocean", "space"], prompts=PROMPTS[:1])
    assert set(r["hubs"]) == {"x0_k8", "x2_k8"}
    base = {s for t in FIT_TOPICS.values() for s in t}
    assert r["hubs"]["x2_k8"]["anchors"] == r["hubs"]["x0_k8"]["anchors"] + len(extra_anchors(2, base))
    assert r["hubs"]["x0_k8"]["anchors"] == 426
    assert all(0 <= v["acc"] <= 1 for c in r["pairs"].values() for v in c.values())
