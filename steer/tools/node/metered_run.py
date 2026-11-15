"""Node-side runner for the metered benchmark: python tools/node/metered_run.py small|big OUT.json [sessions]  (weights from the node's HF cache)."""
import os
import sys
from pathlib import Path

PAIRS = {
    "small": ("HuggingFaceTB/SmolLM2-135M", "Qwen/Qwen3-1.7B-Base", "float32"),
    "big": ("allenai/OLMo-2-1124-7B", "Qwen/Qwen3-8B-Base", "bfloat16"),
}
key, out = sys.argv[1], sys.argv[2]
sessions = int(sys.argv[3]) if len(sys.argv) > 3 else 40
a, b, dt = PAIRS[key]
os.environ.update(PLAYBOX_DEVICE="cuda", TELEPATHY_FIT="agent", TELEPATHY_DTYPE=dt)
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from huggingface_hub import snapshot_download
from playbox_steer import metered_hf as M
pats = ["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"]
pa, pb = (snapshot_download(r, allow_patterns=pats) for r in (a, b))
M.run(pa, pb, n_sessions=sessions, rate_usd_h=3.765, out=Path(out))
print("METERED_DONE", key, flush=True)
