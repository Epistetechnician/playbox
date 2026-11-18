"""The swarm experiment on a plain GPU node, with the real text-swarm arm (every sender writes its own topic, no oracle).

    python tools/telepathy_swarm_node.py OUT_DIR [trials_per_k]     # Qwen3-8B-Base <-> OLMo-2-7B, both directions, bf16, one process
Needs HF_HUB_CACHE with both models; maps go to OUT_DIR/maps (fit takes minutes the first time). Writes OUT_DIR/swarm_cost_<0|1>.json."""
import json
import os
import sys
from pathlib import Path

os.environ.setdefault("PLAYBOX_DEVICE", "cuda")
os.environ.setdefault("TELEPATHY_DTYPE", "bfloat16")
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import torch
from huggingface_hub import snapshot_download
from playbox_steer import telepathy as T
from playbox_steer import telepathy_swarm_run as R

out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
trials = int(sys.argv[2]) if len(sys.argv) > 2 else 100
repos = {"qwen8b": "Qwen/Qwen3-8B-Base", "olmo2": "allenai/OLMo-2-1124-7B"}
models = {n: T.load(snapshot_download(r, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"])) for n, r in repos.items()}
for which in (0, 1):
    res = R.run(models, ("qwen8b", "olmo2"), which, trials, maps_dir=out / "maps", text_agents=True, log=lambda m: print(m, flush=True))
    res.update(gpu=torch.cuda.get_device_name(0), peak_gpu_gb=round(torch.cuda.max_memory_allocated() / 2**30, 2))
    (out / f"swarm_cost_{which}.json").write_text(json.dumps(res))
print("DONE", flush=True)
