"""L4 feasibility test on a Modal A10G:  modal run tools/l4_feasibility_modal.py [--smoke]

Fits a linear map from SmolLM2-135M K/V to Qwen3-1.7B-Base K/V and scores how much of the receiver's own gain from a real
context cache the mapped cache recovers (playbox_steer/l4_feasibility.py). Stops when the function returns; nothing stays up.
"""
import json
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
REPOS = {"sender": "HuggingFaceTB/SmolLM2-135M", "receiver": "Qwen/Qwen3-1.7B-Base"}
app = modal.App("playbox-l4-feasibility")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)
image = (modal.Image.debian_slim(python_version="3.12")
         .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors", "huggingface_hub", "datasets")
         .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"]))


@app.function(image=image, gpu="A10G", volumes={"/vol": vol}, memory=49152, timeout=3600)
def go(smoke: bool) -> dict:
    import os
    import sys
    os.environ.update(HF_HOME="/vol/hf")
    sys.path.insert(0, "/root/svc")
    from huggingface_hub import snapshot_download
    from playbox_steer import l4_feasibility as L
    p = {k: snapshot_download(r, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"]) for k, r in REPOS.items()}
    kw = dict(n_wiki=30, n_templ=30, n_test=12, lams=(0.1,), ranks=(None,)) if smoke else {}
    return L.run(p["sender"], p["receiver"], **kw)


@app.local_entrypoint()
def main(smoke: bool = False):
    r = go.remote(smoke)
    out = STEER / "results/telepathy" / ("l4_smoke.json" if smoke else "l4_feasibility.json")
    out.write_text(json.dumps(r, indent=1))
    print(json.dumps(r, indent=1))
