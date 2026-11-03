"""Run E8 (docs/latent-channels.md) on a GPU node: download five ungated models of five families, run hub_run on CUDA.

    cd ~/e8 && python tools/e8_node.py out/e8.json

Expects playbox_steer/ next to tools/. Needs transformers>=4.51 (Qwen3) and huggingface_hub.
"""
import json
import os
import sys
from pathlib import Path

os.environ.setdefault("PLAYBOX_DEVICE", "cuda")
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from huggingface_hub import snapshot_download  # noqa: E402

REPOS = {
    "smollm2": "HuggingFaceTB/SmolLM2-135M",
    "qwen3": "Qwen/Qwen3-1.7B-Base",
    "pythia": "EleutherAI/pythia-410m",
    "gpt2m": "openai-community/gpt2-medium",
    "tinyllama": "TinyLlama/TinyLlama-1.1B-intermediate-step-1431k-3T",
}


def fetch(repo: str) -> str:
    p = snapshot_download(repo, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"])
    if not list(Path(p).glob("*.safetensors")):
        p = snapshot_download(repo, allow_patterns=["*.json", "*.bin", "*.txt", "*.model", "merges*", "vocab*"])
    return p


def main() -> None:
    out = Path(sys.argv[1])
    out.parent.mkdir(parents=True, exist_ok=True)
    paths = {n: fetch(r) for n, r in REPOS.items()}
    print(json.dumps(paths, indent=1), flush=True)
    from playbox_steer import hub_run

    res = hub_run.run(paths, frac=0.45, k=64, coefs=(0.5,), n_null=6, gen=24)
    out.write_text(json.dumps(res, indent=1))
    print(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
