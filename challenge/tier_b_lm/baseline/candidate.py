"""Pinned baseline: greedy decoding that recomputes the whole sequence every step (no KV cache)."""
import torch
from transformers import AutoModelForCausalLM


class Model:
    def __init__(self, model_dir):
        self.m = AutoModelForCausalLM.from_pretrained(model_dir, torch_dtype=torch.float32).eval()

    def generate(self, ids, n):
        out = list(ids)
        with torch.inference_mode():
            for _ in range(n):
                logits = self.m(torch.tensor([out])).logits[0, -1]
                out.append(int(logits.argmax()))
        return out[len(ids):]
