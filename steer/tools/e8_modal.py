"""E8 on Modal (docs/latent-channels.md): the k sweep and join test, or the main comparison.

    modal run --detach steer/tools/e8_modal.py                 # sweep; survives this client disconnecting
    modal run --detach steer/tools/e8_modal.py --what main     # main comparison
    modal run --detach steer/tools/e8_modal.py --what anchors  # anchor-count sweep (subsets of the 426)
    modal run --detach steer/tools/e8_modal.py --what more     # more anchors than 426 (anchors_extra.py)
    modal run --detach steer/tools/e8_modal.py --what natural  # natural-text anchors and twenty concepts
    modal run --detach steer/tools/e8_modal.py --what chain    # models joining one at a time vs a hub fitted on all
    modal run --detach steer/tools/e8_modal.py --what cards    # concept cards for the closed-model arm (sender side)
    modal run --detach steer/tools/e8_modal.py --what pairs    # per-pair ridge maps on those same anchors (the control)
    modal volume get playbox-e8 e8_sweep.json steer/results/e8/e8_sweep.json

The result is written to the Modal volume `playbox-e8` (which also caches the Hugging Face downloads), so it is kept
even if the local process dies.

Same five ungated models as tools/e8_node.py, fp32 on one GPU, models downloaded in the container from Hugging Face.
"""
import json
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]

FILES = {"sweep": "e8_sweep.json", "main": "e8_main_modal.json", "anchors": "e8_anchors.json", "more": "e8_more_anchors.json", "pairs": "e8_pair_control.json", "cards": "e8_cards.json", "chain": "e8_chain.json", "natural": "e8_natural.json"}
app = modal.App("playbox-e8")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)
image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors", "huggingface_hub", "pandas", "pyarrow")
    .add_local_dir(STEER / "playbox_steer", "/root/e8/playbox_steer", ignore=["__pycache__", "*.pyc"])
    .add_local_file(STEER / "tools/e8_node.py", "/root/e8/tools/e8_node.py")  # only what is imported: other files in tools/ change under us
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
    elif what == "natural":
        from huggingface_hub import hf_hub_download
        import pandas as pd

        f = hf_hub_download("Salesforce/wikitext", "wikitext-2-raw-v1/train-00000-of-00001.parquet", repo_type="dataset")
        res = hub_run.natural_eval(paths, [t for t in pd.read_parquet(f)["text"].tolist() if t.strip()])
    elif what == "chain":
        res = hub_run.join_chain(paths)
    elif what == "cards":
        res = hub_run.cards(paths)
    elif what == "pairs":
        res = hub_run.pair_control(paths)
    elif what == "more":
        res = hub_run.anchor_extend(paths)
    elif what == "anchors":
        res = hub_run.anchor_sweep(paths)
    else:
        res = hub_run.sweep(paths)
    name = FILES[what]
    Path(f"/vol/{name}").write_text(json.dumps(res, indent=1))
    vol.commit()
    return res


@app.local_entrypoint()
def main(what: str = "sweep"):
    if what not in FILES:
        raise SystemExit("--what is " + ", ".join(FILES))
    res = run.remote(what)
    out = STEER / "results/e8" / FILES[what]
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(res, indent=1))
    print("wrote", out)
