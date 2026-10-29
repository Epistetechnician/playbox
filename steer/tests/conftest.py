import json
from pathlib import Path

import pytest
import torch
from transformers import GPT2Config, GPT2LMHeadModel, LlamaConfig, LlamaForCausalLM

from playbox_steer import files


def _tokenizer_dir() -> Path:
    from huggingface_hub import snapshot_download

    return Path(snapshot_download("openai-community/gpt2", allow_patterns=["vocab.json", "merges.txt", "tokenizer*.json", "config.json"]))


def _save(model, path: Path, tok_dir: Path):
    model.save_pretrained(path, safe_serialization=True)
    from transformers import GPT2TokenizerFast

    GPT2TokenizerFast.from_pretrained(tok_dir).save_pretrained(path)


@pytest.fixture(scope="session")
def tok_dir():
    return _tokenizer_dir()


@pytest.fixture(scope="session")
def gpt2_dir(tmp_path_factory, tok_dir):
    torch.manual_seed(0)
    m = GPT2LMHeadModel(GPT2Config(n_layer=4, n_embd=32, n_head=2, n_positions=128, vocab_size=50257))
    p = tmp_path_factory.mktemp("tiny-gpt2")
    _save(m, p, tok_dir)
    return p


@pytest.fixture(scope="session")
def llama_dir(tmp_path_factory, tok_dir):
    torch.manual_seed(0)
    m = LlamaForCausalLM(LlamaConfig(hidden_size=32, intermediate_size=64, num_hidden_layers=4, num_attention_heads=2, num_key_value_heads=2, vocab_size=50257, max_position_embeddings=128))
    p = tmp_path_factory.mktemp("tiny-llama")
    _save(m, p, tok_dir)
    return p


PROMPTS = {"prompts": ["The weather today is", "I think that", "Once upon a time"], "max_new_tokens": 6, "temperature": 0.0}


def make_task(tmp: Path, weights: Path, *, site="resid_post", layers=(1, 2), mode="add", coef_milli=4000, tokens="all", seed=1, direction=None, prompts=PROMPTS):
    """Write spec.txt, direction.f32 and prompts.json for a run and return (task_dir, spec_text)."""
    from playbox_steer import engine

    tmp.mkdir(parents=True, exist_ok=True)
    model, _ = engine.load(weights)
    dim = model.config.hidden_size
    if direction is None:
        direction = engine.random_direction(dim, 123)
    dhash = files.write_direction(tmp / "direction.f32", direction)
    (tmp / "prompts.json").write_text(json.dumps(prompts))
    phash = files.hash_file(tmp / "prompts.json")
    spec = "".join(
        f"{k} {v}\n"
        for k, v in [
            ("model", "tiny"),
            ("weights", files.weights_manifest(weights)),
            ("site", site),
            ("layers", ",".join(map(str, layers))),
            ("direction", dhash),
            ("recipe", "diffmean"),
            ("mode", mode),
            ("coef_milli", coef_milli),
            ("tokens", tokens),
            ("prompts", phash),
            ("seed", seed),
        ]
    )
    (tmp / "spec.txt").write_text(spec)
    return tmp, spec
