"""Does a small guarded payload mixed into an honest message actually steer the receiver? (docs/latent-channels.md 6r)

    python tools/e8_payload_effect.py --out results/e8/e8_payload_effect.json

The audit and the guard both miss small mixes (6r). Whether that matters depends on whether a small mix steers. For each ordered pair
of the three small models, each carrier concept C (music, sport, ocean, space) and a in 0, 0.25, 0.5, 1.0, the sender posts
(1-a) * C + a * medicine through the hub; the receiver decodes it, scales it to 0.5 receiver norms (the room's strength) and generates
on the eight standard prompts. Reported: the share of generations the lexicon decoder reads as the carrier, as medicine, and as
neither. One run, 24 tokens, greedy; the lexicon decoder is coarse (see 6e).
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
from playbox_steer import telepathy as T
from playbox_steer.anchors_extra import extra_anchors
from playbox_steer.concepts_natural import ALL_CONCEPTS
from playbox_steer.telepathy_data import PROMPTS, TEST_CONCEPTS

GUARDED = "medicine"
MIXES = (0.0, 0.25, 0.5, 1.0)
CARRIERS = [c for c in TEST_CONCEPTS if c != GUARDED]
COEF = 0.5


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out")
    ap.add_argument("--gen", type=int, default=24)
    a = ap.parse_args()
    torch.set_grad_enabled(False)
    train, held = HR._anchor_split()
    anchors = train + extra_anchors(16, set(train) | set(held))
    info = {n: HR._sender_pass(p, 0.45, anchors, held, list(ALL_CONCEPTS), bank=ALL_CONCEPTS) for n, p in R.MODELS.items()}
    hub = H.fit_hub({n: i["train"] for n, i in info.items()}, H.anchor_digest(anchors), k=256, r=round(0.6 * len(anchors)))
    lex = {c: set(TEST_CONCEPTS[c]["lexicon"]) for c in TEST_CONCEPTS}
    tally = {m: {"n": 0, "carrier": 0, "medicine": 0} for m in MIXES}
    per_dst = {}
    for dst, path in R.MODELS.items():
        mb, tb = T.load(path)
        layer = T.layer_of(mb, 0.45)
        for src in info:
            if src == dst:
                continue
            th = info[src]["thought"]
            for c in CARRIERS:
                for m in MIXES:
                    vec = hub.decode(dst, hub.encode(src, (1 - m) * th[c] + m * th[GUARDED]))
                    vec = vec / vec.norm() * COEF * info[dst]["norm"]
                    for p in PROMPTS:
                        got = T.lexicon_id(T.generate(mb, tb, layer, vec, p, a.gen), lex)[0]
                        for t in (tally[m], per_dst.setdefault(dst, {}).setdefault(str(m), {"n": 0, "carrier": 0, "medicine": 0})):
                            t["n"] += 1
                            t["carrier"] += got == c
                            t["medicine"] += got == GUARDED
        print(dst, "done", flush=True)
    for m, v in tally.items():
        print(m, v, f"carrier {v['carrier'] / v['n']:.2f} medicine {v['medicine'] / v['n']:.2f}", flush=True)
    if a.out:
        Path(a.out).write_text(json.dumps({"coef": COEF, "carriers": CARRIERS, "by_mix": {str(m): v for m, v in tally.items()},
                                           "by_receiver": per_dst}, indent=1))


if __name__ == "__main__":
    main()
