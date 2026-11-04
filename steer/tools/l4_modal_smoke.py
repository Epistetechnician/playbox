"""Debug the L4 trainer for a few steps on a Modal A10G (cheap):  modal run tools/l4_modal_smoke.py"""
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
app = modal.App("playbox-l4-smoke")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)
image = (modal.Image.debian_slim(python_version="3.12")
         .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors", "huggingface_hub", "datasets")
         .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"]))


@app.function(image=image, gpu="A10G", volumes={"/vol": vol}, memory=32768, timeout=1800)
def go(direction: str, s1: int, s2: int, items: int, blind: bool = False) -> list:
    import os
    import sys
    os.environ.update(HF_HOME="/vol/hf")
    sys.path.insert(0, "/root/svc")
    from huggingface_hub import snapshot_download
    from playbox_steer import l4_swat as S
    g = lambda r: snapshot_download(r, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"])
    small, big = g("HuggingFaceTB/SmolLM2-135M"), g("Qwen/Qwen3-1.7B-Base")
    snd, rcv = (small, big) if direction == "s2q" else (big, small)
    hist = S.train(snd, rcv, "/tmp/smoke.pt", steps1=s1, steps2=s2, bs=16, n_items=items, B=3, D=512, eval_every=100, blind=blind)
    from playbox_steer import l4_eval as E
    return {"hist": hist[-1:], "eval": E.run("/tmp/smoke.pt", n_wiki=12)}


@app.local_entrypoint()
def main(direction: str = "s2q", s1: int = 20, s2: int = 20, items: int = 600, blind: bool = False):
    print(go.remote(direction, s1, s2, items, blind))
