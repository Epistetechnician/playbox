"""Run E6 on a real model on CPU:  python -m playbox_steer.e6 --weights DIR --out report.json

Builds a sentiment direction at the steering layer and another at the probe layer from contrasting sentences
(the same ones E1 uses), seeded random null directions, and neutral contexts.
"""
from __future__ import annotations

import argparse
import json
import random
from pathlib import Path

import torch

from . import engine, latent

POSITIVE = [
    "I absolutely love this, it is wonderful", "What a fantastic, joyful day", "This is the best thing ever, I adore it",
    "I am so happy and grateful for everything", "The food was delicious and the staff were delightful",
    "What a beautiful, brilliant performance", "I feel great, everything is going perfectly", "She is kind, generous and a pleasure to know",
    "An amazing, uplifting, truly lovely experience", "We had a wonderful time and loved every minute",
    "He did an excellent job and we are thrilled", "Such a charming, cheerful and warm place",
]
NEGATIVE = [
    "I absolutely hate this, it is terrible", "What an awful, miserable day", "This is the worst thing ever, I despise it",
    "I am so sad and resentful about everything", "The food was disgusting and the staff were rude",
    "What an ugly, dreadful performance", "I feel awful, everything is going badly", "She is cruel, selfish and a pain to deal with",
    "A horrible, depressing, truly miserable experience", "We had a terrible time and hated every minute",
    "He did a poor job and we are furious", "Such a gloomy, hostile and cold place",
]
SUBJECTS = ["movie", "food", "hotel", "phone", "meeting", "album", "weather", "book"]
FRAMES = ["The {} was", "My honest opinion of the {} is", "Today the {} seems", "Everyone says the {} is",
          "I just heard that the {} was", "Overall the {} turned out", "People describe the {} as", "After a week, the {} felt"]
QUERY = " Honestly, I think it was"


def contexts(n: int) -> list[str]:
    grid = [f.format(s) for f in FRAMES for s in SUBJECTS]
    random.Random(0).shuffle(grid)
    return grid[:n]


def main(argv=None) -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--weights", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--steer-layer", type=int, default=4)
    ap.add_argument("--probe-layer", type=int, default=9)
    ap.add_argument("--coefs", default="0.5,1,2,4")
    ap.add_argument("--nulls", type=int, default=8)
    ap.add_argument("--contexts", type=int, default=32)
    a = ap.parse_args(argv)
    w = Path(a.weights)
    model, tok = engine.load(w)
    dim = model.config.hidden_size
    trait = torch.tensor(engine.extract_diffmean(w, POSITIVE, NEGATIVE, "resid_post", a.steer_layer))
    probe = torch.tensor(engine.extract_diffmean(w, POSITIVE, NEGATIVE, "resid_post", a.probe_layer))
    nulls = [torch.tensor(engine.random_direction(dim, 100 + k)) for k in range(a.nulls)]
    out = {"model": str(w), "runs": {}}
    for c in [float(x) for x in a.coefs.split(",")]:
        r = latent.run_e6(model, tok, contexts(a.contexts), QUERY, trait, nulls, a.steer_layer, probe, a.probe_layer, c)
        out["runs"][str(c)] = r
        s, t = r["arms"]["steered"], r["arms"]["text_steered"]
        print(f"coef {c:>4}: latent trait-logit {s['trait_logit_effect']['mean']:+.3f} "
              f"[{s['trait_logit_effect']['low']:+.3f},{s['trait_logit_effect']['high']:+.3f}] KL {s['kl_mean']:.3f} | "
              f"text channel {t['trait_logit_effect']['mean']:+.3f} | nulls >= steered: {r.get('null_exceed_fraction', float('nan')):.2f}", flush=True)
    Path(a.out).write_text(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
