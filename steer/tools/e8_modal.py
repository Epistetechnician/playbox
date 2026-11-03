"""E8 on Modal (docs/latent-channels.md): the k sweep and join test, or the main comparison.

    modal run --detach steer/tools/e8_modal.py                 # sweep; survives this client disconnecting
    modal run --detach steer/tools/e8_modal.py --what main     # main comparison
    modal volume get playbox-e8 e8_sweep.json steer/results/e8/e8_sweep.json

The result is written to the Modal volume `playbox-e8` (which also caches the Hugging Face downloads), so it is kept
even if the local process dies.

Same five ungated models as tools/e8_node.py, fp32 on one GPU, models downloaded in the container from Hugging Face.
"""
import json
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]

app = modal.App("playbox-e8")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)
image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors", "huggingface_hub")
    .add_local_dir(STEER / "playbox_steer", "/root/e8/playbox_steer", ignore=["__pycache__", "*.pyc"])
    .add_local_dir(STEER / "tools", "/root/e8/tools", ignore=["__pycache__", "*.pyc", "hermes_local", "hermes_mlx"])
)


@app.function(image=image, gpu="A10G", timeout=3 * 3600, volumes={"/vol": vol})
def run(what: str) -> dict:
    import os
    import sys

    os.environ["PLAYBOX_DEVICE"] = "cuda"
    os.environ["HF_HOME"] = "/vol/hf"
    sys.path[:0] = ["/root/e8", "/root/e8/tools"]
    import torch
    from e8_node import REPOS, fetch

    print("cuda", torch.cuda.is_available(), torch.cuda.get_device_name(0), flush=True)
    paths = {n: fetch(r) for n, r in REPOS.items()}
    from playbox_steer import hub_run

    if what == "main":
        res = hub_run.run(paths, frac=0.45, k=64, coefs=(0.5,), n_null=6, gen=24)
    else:
        res = hub_run.sweep(paths)
    name = "e8_sweep.json" if what == "sweep" else "e8_main_modal.json"
    Path(f"/vol/{name}").write_text(json.dumps(res, indent=1))
    vol.commit()
    return res


@app.local_entrypoint()
def main(what: str = "sweep"):
    if what not in ("sweep", "main"):
        raise SystemExit("--what is sweep or main")
    res = run.remote(what)
    out = STEER / "results/e8" / ("e8_sweep.json" if what == "sweep" else "e8_main_modal.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(res, indent=1))
    print("wrote", out)
