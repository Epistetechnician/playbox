"""Train one direction of the SwaT-style cache mapper:
    python tools/l4_train.py --sender HuggingFaceTB/SmolLM2-135M --receiver Qwen/Qwen3-1.7B-Base --out ckpt/s2q.pt [--steps1 N --steps2 N]
Paths may be local directories or hub ids resolved through the Hugging Face cache. Logs go to stdout (no credentials in them)."""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_steer import l4_swat as S  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--sender", required=True)
ap.add_argument("--receiver", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--steps1", type=int, default=3000)
ap.add_argument("--steps2", type=int, default=8000)
ap.add_argument("--bs", type=int, default=48)
ap.add_argument("--items", type=int, default=100000)
ap.add_argument("--blocks", type=int, default=7)
ap.add_argument("--width", type=int, default=1024)
ap.add_argument("--eval-every", type=int, default=1000)
ap.add_argument("--budget-s", type=float, default=None)
ap.add_argument("--templ-frac", type=float, default=0.15, help="share of templated topical items in the training mix")
ap.add_argument("--blind", action="store_true", help="text-blind mapper: no receiver token identities, lengths only")
a = ap.parse_args()
Path(a.out).parent.mkdir(parents=True, exist_ok=True)
hist = S.train(a.sender, a.receiver, a.out, a.steps1, a.steps2, a.bs, n_items=a.items, B=a.blocks, D=a.width,
               eval_every=a.eval_every, time_budget_s=a.budget_s, blind=a.blind, templ_frac=a.templ_frac, log=lambda *x: print(*x, flush=True))
print(json.dumps(hist[-1] if hist else {}))
