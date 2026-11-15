"""The metered shared-task benchmark on Modal: every channel (text word, RAG, cached working set, latent vector, routed swarm) on one task, GPU-seconds per arm.

    modal run --detach steer/tools/metered_bench_modal.py [--pairs small,big] [--sessions 40]
    modal volume get playbox-telepathy-bench /results/metered_small.json steer/results/metered/ --force
    python steer/tools/metered_bench_report.py steer/results/metered/metered_small.json [--rate 1.10]

small: SmolLM2-135M (sender) -> Qwen3-1.7B-Base (receiver), A10G, float32.  big: OLMo-2-7B (sender) -> Qwen3-8B-Base (receiver), H100, bfloat16.
The experiment is playbox_steer/metered_hf.py; the meter and the task are playbox_steer/metered_bench.py. Detached, result left on the volume."""
import json
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
PAIRS = {
    "small": {"sender": "HuggingFaceTB/SmolLM2-135M", "receiver": "Qwen/Qwen3-1.7B-Base", "dtype": "float32", "rate": 1.10},
    "big": {"sender": "allenai/OLMo-2-1124-7B", "receiver": "Qwen/Qwen3-8B-Base", "dtype": "bfloat16", "rate": 3.95},
}
app = modal.App("playbox-metered-bench")
vol = modal.Volume.from_name("playbox-telepathy-bench", create_if_missing=True)
image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors", "huggingface_hub")
    .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"])
)


def _run(key: str, sessions: int) -> dict:
    import os
    import sys
    cfg = PAIRS[key]
    os.environ.update(PLAYBOX_DEVICE="cuda", HF_HOME="/vol/hf", TELEPATHY_FIT="agent", TELEPATHY_DTYPE=cfg["dtype"])
    sys.path.insert(0, "/root/svc")
    from huggingface_hub import snapshot_download
    from playbox_steer import metered_hf as M
    pats = ["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"]
    a, b = (snapshot_download(cfg[k], allow_patterns=pats) for k in ("sender", "receiver"))
    out = Path(f"/vol/results/metered_{key}.json")
    out.parent.mkdir(exist_ok=True)
    r = M.run(a, b, n_sessions=sessions, rate_usd_h=cfg["rate"], out=out)
    vol.commit()
    return r


@app.function(image=image, gpu="A10G", volumes={"/vol": vol}, timeout=7200)
def small(sessions: int) -> dict:
    return _run("small", sessions)


@app.function(image=image, gpu="H100", volumes={"/vol": vol}, timeout=7200)
def big(sessions: int) -> dict:
    return _run("big", sessions)


@app.local_entrypoint()
def main(pairs: str = "small", sessions: int = 40):
    fns = {"small": small, "big": big}
    calls = {k: fns[k].spawn(sessions) for k in pairs.split(",")}
    outdir = STEER / "results/metered"
    outdir.mkdir(exist_ok=True)
    for k, c in calls.items():
        for attempt in range(4):
            try:
                (outdir / f"metered_{k}.json").write_text(json.dumps(c.get(), indent=1))
                break
            except Exception as e:
                print("RETRY" if attempt < 3 else "FAILED", k, repr(e)[:200], flush=True)
