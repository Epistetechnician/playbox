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
TRAITS = {
    "sentiment": {
        "positive": POSITIVE, "negative": NEGATIVE, "query": " Honestly, I think it was",
        "pos_words": latent.POSITIVE_WORDS, "neg_words": latent.NEGATIVE_WORDS,
    },
    "fear": {
        "positive": [
            "I was terrified and my heart was pounding", "She felt a cold dread creeping up her spine", "We were all scared of what might happen",
            "He was anxious and trembling with fear", "The dark hallway made me panic", "They fled in horror from the danger",
            "I am worried sick that something terrible will happen", "A chilling threat hung over the whole town", "The children were frightened and hid",
            "Everyone was nervous and on edge", "It was a terrifying, dangerous night", "He dreaded every step toward the cliff",
        ],
        "negative": [
            "I was relaxed and my heart was calm", "She felt a warm comfort settle over her", "We were all certain that things would be fine",
            "He was cheerful and at ease", "The bright hallway made me smile", "They strolled home feeling safe",
            "I am happy that everything will go smoothly", "A gentle peace hung over the whole town", "The children were playful and laughed",
            "Everyone was confident and at rest", "It was a peaceful, harmless evening", "He enjoyed every step along the meadow",
        ],
        "query": " Right now I feel",
        "pos_words": [" afraid", " scared", " terrified", " anxious", " nervous", " worried", " frightened", " uneasy"],
        "neg_words": [" calm", " safe", " relaxed", " comfortable", " peaceful", " fine", " happy", " secure"],
    },
}
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
    ap.add_argument("--coefs", default="0.25,0.5,1")
    ap.add_argument("--nulls", type=int, default=32)
    ap.add_argument("--contexts", type=int, default=64)
    ap.add_argument("--traits", default="sentiment,fear")
    a = ap.parse_args(argv)
    w = Path(a.weights)
    model, tok = engine.load(w)
    dim = model.config.hidden_size
    nulls = [torch.tensor(engine.random_direction(dim, 100 + k)) for k in range(a.nulls)]
    out = {"model": str(w), "steer_layer": a.steer_layer, "probe_layer": a.probe_layer, "traits": {}}
    for name in a.traits.split(","):
        t = TRAITS[name]
        trait = torch.tensor(engine.extract_diffmean(w, t["positive"], t["negative"], "resid_post", a.steer_layer))
        probe = torch.tensor(engine.extract_diffmean(w, t["positive"], t["negative"], "resid_post", a.probe_layer))
        out["traits"][name] = {}
        for c in [float(x) for x in a.coefs.split(",")]:
            r = latent.run_e6(model, tok, contexts(a.contexts), t["query"], trait, nulls, a.steer_layer, probe, a.probe_layer, c,
                              positive_words=t["pos_words"], negative_words=t["neg_words"])
            out["traits"][name][str(c)] = r
            s_ = r["arms"]["steered"]
            sp = r["specific_effect"]
            print(f"{name:9} coef {c:>4}: latent {s_['trait_logit_effect']['mean']:+.3f} | trait-specific (vs null mean) {sp['mean']:+.3f} "
                  f"[{sp['low']:+.3f},{sp['high']:+.3f}] | nulls pushing as far same way {r['null_signed_exceed_fraction']:.2f} | "
                  f"KL {s_['kl_mean']:.3f} (null mean KL {sum(r['arms'][k]['kl_mean'] for k in r['arms'] if k.startswith('null_'))/a.nulls:.3f}) | "
                  f"text channel {r['arms']['text_steered']['trait_logit_effect']['mean']:+.3f}", flush=True)
    Path(a.out).write_text(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
