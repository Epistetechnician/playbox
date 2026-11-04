"""Run the fit-data scaling test on a Modal GPU (both directions):  modal run tools/telepathy_scale_modal.py"""
import json
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
REPOS = {"smollm2": "HuggingFaceTB/SmolLM2-135M", "qwen3": "Qwen/Qwen3-1.7B-Base"}
app = modal.App("playbox-telepathy-scale")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)
image = (modal.Image.debian_slim(python_version="3.12")
         .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors", "huggingface_hub")
         .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"]))


@app.function(image=image, gpu="A10G", volumes={"/vol": vol}, timeout=3600)
def scale() -> dict:
    import os
    import sys
    os.environ.update(PLAYBOX_DEVICE="cuda", HF_HOME="/vol/hf")
    sys.path.insert(0, "/root/svc")
    from huggingface_hub import snapshot_download
    from playbox_steer import telepathy as T
    from playbox_steer import telepathy_scale as S
    m = {k: T.load(snapshot_download(r, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"]))
         for k, r in REPOS.items()}
    return {f"{a}->{b}": S.run(*m[a], *m[b]) for a, b in (("smollm2", "qwen3"), ("qwen3", "smollm2"))}


@app.local_entrypoint()
def main():
    r = scale.remote()
    out = STEER / "results/telepathy/scale.json"
    out.write_text(json.dumps(r, indent=1))
    for d, rows in r.items():
        print(d)
        for x in rows:
            print(" ", x)
