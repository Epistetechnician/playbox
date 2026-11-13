"""Cross-family latent swarm on Modal: n sender agents in one family, one summarizer in another, one mapped vector between them.

    modal run --detach steer/tools/telepathy_swarm_modal.py [--pairs small,big] [--trials 60] [--big-in-one]
    modal volume get playbox-telepathy-bench /results/ steer/results/telepathy/swarm/ --force
    python steer/tools/telepathy_swarm_report.py

One container per pair and direction (small pair on an A10G, Qwen3-8B/OLMo-2-7B in bf16 on an H100), detached so a dropped client
connection cannot stop it; each result is left on the volume. The experiment is in playbox_steer/telepathy_swarm_run.py.
Maps are cached on the volume, so a rerun skips the 11-minute fit."""
import json
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
PAIRS = {
    "small": {"repos": {"smollm2": "HuggingFaceTB/SmolLM2-135M", "qwen3": "Qwen/Qwen3-1.7B-Base"}, "dtype": "float32"},
    "big": {"repos": {"qwen8b": "Qwen/Qwen3-8B-Base", "olmo2": "allenai/OLMo-2-1124-7B"}, "dtype": "bfloat16"},
}
app = modal.App("playbox-telepathy-swarm")
vol = modal.Volume.from_name("playbox-telepathy-bench", create_if_missing=True)  # same volume as the benchmark: HF cache and maps
image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors", "huggingface_hub")
    .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"])
)


def _swarm(key: str, whichs: tuple, trials: int) -> dict:
    import os
    import sys
    import time
    cfg = PAIRS[key]
    os.environ.update(PLAYBOX_DEVICE="cuda", HF_HOME="/vol/hf", TELEPATHY_DTYPE=cfg["dtype"])
    sys.path.insert(0, "/root/svc")
    import torch
    from huggingface_hub import snapshot_download
    from playbox_steer import telepathy as T
    from playbox_steer import telepathy_swarm_run as R

    t0 = time.perf_counter()
    models = {n: T.load(snapshot_download(r, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"]))
              for n, r in cfg["repos"].items()}
    cold = round(time.perf_counter() - t0, 1)
    saved = []
    for which in whichs:  # both directions in one container: the models are loaded once and the maps come from the volume
        res = R.run(models, tuple(cfg["repos"]), which, trials, n_agents=6, ks=(2, 3), maps_dir=Path("/vol/maps"), fit="agent",
                    log=lambda s: print(s, flush=True))
        res.update(pair=key, dtype=cfg["dtype"], gpu=torch.cuda.get_device_name(0), cold_start_s=cold,
                   peak_gpu_gb=round(torch.cuda.max_memory_allocated() / 2**30, 2))
        os.makedirs("/vol/results", exist_ok=True)
        Path(f"/vol/results/swarm_{key}_{which}.json").write_text(json.dumps(res))
        vol.commit()
        saved.append(f"/results/swarm_{key}_{which}.json")
    return {"saved": saved}


@app.function(image=image, gpu="A10G", volumes={"/vol": vol}, timeout=7200)
def swarm_small(which: int, trials: int) -> dict:
    return _swarm("small", (which,), trials)


@app.function(image=image, gpu="H100", volumes={"/vol": vol}, timeout=7200)
def swarm_big(which: int, trials: int) -> dict:
    return _swarm("big", (which,), trials)


@app.function(image=image, gpu=["H100", "A100-80GB", "L40S"], volumes={"/vol": vol}, timeout=14400)
def swarm_big_both(trials: int) -> dict:
    """The larger pair, both directions one after the other in one container (any of the listed GPUs: 29 GB of models fit on all)."""
    return _swarm("big", (0, 1), trials)


@app.local_entrypoint()
def main(pairs: str = "small,big", trials: int = 60, big_in_one: bool = False):
    if big_in_one:  # one GPU container instead of two H100s: the workspace's H100 concurrency limit stalled the two-container run
        print(swarm_big_both.spawn(trials).object_id)
        return
    fns = {"small": swarm_small, "big": swarm_big}
    calls = {(k, w): fns[k].spawn(w, trials) for k in pairs.split(",") for w in (0, 1)}
    for (k, w), c in calls.items():
        try:
            print(k, w, c.get(timeout=60))
        except Exception as e:  # the container keeps running detached; fetch from the volume later
            print("not waiting on", k, w, repr(e)[:120])
