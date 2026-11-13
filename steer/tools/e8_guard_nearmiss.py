"""Does the receiver's guard (6g) stop a guarded payload mixed into an honest message? (docs/latent-channels.md 6r)

    python tools/e8_guard_nearmiss.py --out results/e8/e8_guard_nearmiss.json

For every sender, receiver and committed concept C (the 15 natural concepts plus the 5 template ones except the guarded
`medicine`), the sender posts (1-a) * C + a * medicine through the hub, the receiver decodes it into its own space and applies
the guard exactly as `latent_room.guard_blocks` does (|cosine| with the receiver's own `medicine` direction >= 0.5).
Honest messages of the same concepts give the false-block rate. a in 0, 0.1, 0.25, 0.5, 0.75, 1.0 (a=1 is a plain medicine message).
"""
import argparse
import json
import os
import sys
from pathlib import Path

os.environ.setdefault("HF_HUB_OFFLINE", "1")
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import torch

import e8_room_demo as R
from playbox_steer import hub as H
from playbox_steer import hub_run as HR
from playbox_steer import latent_room as LR
from playbox_steer.anchors_extra import extra_anchors
from playbox_steer.concepts_natural import ALL_CONCEPTS

GUARDED = "medicine"
MIXES = (0.0, 0.1, 0.25, 0.5, 0.75, 1.0)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out")
    a = ap.parse_args()
    torch.set_grad_enabled(False)
    train, held = HR._anchor_split()
    anchors = train + extra_anchors(16, set(train) | set(held))
    info = {n: HR._sender_pass(p, 0.45, anchors, held, list(ALL_CONCEPTS), bank=ALL_CONCEPTS) for n, p in R.MODELS.items()}
    hub = H.fit_hub({n: i["train"] for n, i in info.items()}, H.anchor_digest(anchors), k=256, r=round(0.6 * len(anchors)))
    concepts = [c for c in ALL_CONCEPTS if c != GUARDED]
    cell = {m: {"n": 0, "blocked": 0, "cos": []} for m in MIXES}
    for src in info:
        for dst in info:
            if src == dst:
                continue
            guards = {"g": (info[dst]["thought"][GUARDED], GUARDED)}
            for c in concepts:
                th = info[src]["thought"]
                for m in MIXES:
                    vec = hub.decode(dst, hub.encode(src, (1 - m) * th[c] + m * th[GUARDED]))
                    v = torch.nn.functional.normalize(vec.float(), dim=-1)
                    d = torch.nn.functional.normalize(guards["g"][0].float(), dim=-1)
                    cell[m]["n"] += 1
                    cell[m]["blocked"] += bool(LR.guard_blocks(vec, guards, {GUARDED}))
                    cell[m]["cos"].append(float(v @ d))
    out = {}
    for m, v in cell.items():
        xs = sorted(v["cos"])
        out[str(m)] = {"n": v["n"], "blocked": v["blocked"], "median_cos": round(xs[len(xs) // 2], 3), "max_cos": round(xs[-1], 3)}
        print(m, out[str(m)], flush=True)
    if a.out:
        Path(a.out).write_text(json.dumps({"guarded": GUARDED, "threshold": LR.GUARD_COS_THRESHOLD, "by_mix": out}, indent=1))


if __name__ == "__main__":
    main()
