"""Write a small random GPT-2 (with the real GPT-2 tokenizer) for tests: python make_tiny_model.py OUT_DIR."""
import sys
from pathlib import Path

import torch
from huggingface_hub import snapshot_download
from transformers import GPT2Config, GPT2LMHeadModel, GPT2TokenizerFast

out = Path(sys.argv[1])
tok = snapshot_download("openai-community/gpt2", allow_patterns=["vocab.json", "merges.txt", "tokenizer*.json", "config.json"])
torch.manual_seed(0)
GPT2LMHeadModel(GPT2Config(n_layer=4, n_embd=32, n_head=2, n_positions=128, vocab_size=50257)).save_pretrained(out, safe_serialization=True)
GPT2TokenizerFast.from_pretrained(tok).save_pretrained(out)
