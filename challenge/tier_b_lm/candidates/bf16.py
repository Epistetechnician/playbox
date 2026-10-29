"""Lossy shortcut: bf16 weights. Fast, but not token-identical, so the lossless track must reject it."""
import torch
from transformers import AutoModelForCausalLM


class Model:
    def __init__(self, model_dir):
        self.m = AutoModelForCausalLM.from_pretrained(model_dir, torch_dtype=torch.bfloat16).eval()

    def generate(self, ids, n):
        out = []
        with torch.inference_mode():
            r = self.m(torch.tensor([ids]), use_cache=True)
            for _ in range(n):
                nxt = int(r.logits[0, -1].argmax())
                out.append(nxt)
                r = self.m(torch.tensor([[nxt]]), past_key_values=r.past_key_values, use_cache=True)
        return out
