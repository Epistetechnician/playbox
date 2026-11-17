"""Train the v2 blind mapper (ridge start, KL loss, key weight, edge recompute); each part is a switch so it can be ablated:
    python tools/l4_train_v2.py --sender Qwen/Qwen3-4B-Base --receiver unsloth/Llama-3.2-3B --out ckpt/v2_s0.pt --seed 0 [--no-ridge] [--loss nll] [--key-weight 1] [--edge-k 0]
Same data, steps and mapper size as tools/l4_train.py with --blind --blocks 4 --width 768 (the section 11 and 12 recipe)."""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_steer import l4_swat_v2 as V  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--sender", required=True)
ap.add_argument("--receiver", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--seed", type=int, default=0)
ap.add_argument("--steps1", type=int, default=1500)
ap.add_argument("--steps2", type=int, default=4000)
ap.add_argument("--bs", type=int, default=32)
ap.add_argument("--items", type=int, default=50000)
ap.add_argument("--blocks", type=int, default=4)
ap.add_argument("--width", type=int, default=768)
ap.add_argument("--eval-every", type=int, default=500)
ap.add_argument("--budget-s", type=float, default=None)
ap.add_argument("--no-ridge", action="store_true")
ap.add_argument("--loss", choices=("kl", "nll"), default="kl")
ap.add_argument("--key-weight", type=float, default=2.0)
ap.add_argument("--edge-k", type=int, default=4)
ap.add_argument("--ridge-batches", type=int, default=160)
a = ap.parse_args()
Path(a.out).parent.mkdir(parents=True, exist_ok=True)
cfg = V.V2Config(ridge=not a.no_ridge, loss=a.loss, key_weight=a.key_weight, edge_k=a.edge_k, ridge_batches=a.ridge_batches)
hist = V.train(a.sender, a.receiver, a.out, cfg, steps1=a.steps1, steps2=a.steps2, bs=a.bs, n_items=a.items, B=a.blocks, D=a.width,
               eval_every=a.eval_every, time_budget_s=a.budget_s, seed=a.seed, log=lambda *x: print(*x, flush=True))
print(json.dumps(hist[-1] if hist else {}))
