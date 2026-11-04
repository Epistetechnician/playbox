"""E8 token cost (docs/latent-channels.md 6j): what a concept costs to send as text versus as a hub vector, same receivers.

    python tools/e8_token_cost.py --out results/e8/e8_token_cost.json

For each of the three local receivers (SmolLM2-135M, Pythia-410m, GPT-2), the same 5 concepts and 8 prompts, greedy 24 tokens,
scored by the same lexicon decoder as every other E8 run. Arms:
    none      the prompt alone
    words     6 key words (tf-idf from the 8 sentences) put in front of the prompt as text
    sentences the 8 sentences the sender read, put in front of the prompt as text
    latent    the hub vector from another model (results/e8/e8_room.json, 5 concepts x 2 senders x 8 prompts, ungated)
    direct    the receiver's own direction at the same strength (upper bound; from the same file)
Reported per arm: tokens added to the receiver's context, bytes sent, and accuracy.
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
from playbox_steer import hub_run as HR
from playbox_steer import telepathy as T
from playbox_steer.anchors_extra import extra_anchors
from playbox_steer.telepathy_data import PROMPTS, TEST_CONCEPTS


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--room", default=str(Path(__file__).resolve().parents[1] / "results/e8/e8_room.json"))
    ap.add_argument("--gen", type=int, default=24)
    ap.add_argument("--out")
    a = ap.parse_args()
    torch.set_grad_enabled(False)
    room = json.loads(Path(a.room).read_text())
    concepts = list(TEST_CONCEPTS)
    lex = {c: set(TEST_CONCEPTS[c]["lexicon"]) for c in concepts}
    train, held = HR._anchor_split()
    corpus = train + extra_anchors(16, set(train) | set(held)) + [t for c in TEST_CONCEPTS.values() for t in c["sentences"]]
    ctx = {"words": {c: HR.text_baseline(TEST_CONCEPTS[c]["sentences"], 6, corpus) for c in concepts},
           "sentences": {c: " ".join(TEST_CONCEPTS[c]["sentences"]) for c in concepts}}
    env_bytes = sum(room["envelope_bytes"]) / len(room["envelope_bytes"])

    out: dict = {"per_receiver": {}, "envelope_bytes": round(env_bytes), "chance": 1 / len(concepts)}
    totals: dict[str, list[int]] = {}
    for dst, path in R.MODELS.items():
        mb, tb = T.load(path)
        layer = T.layer_of(mb, 0.45)
        arms: dict[str, dict] = {}

        def run(arm: str, context: dict | None):
            hits = n = toks = 0
            for c in concepts:
                head = "" if context is None else context[c] + "\n\n"
                toks += len(tb(head).input_ids) if head else 0
                for p in PROMPTS:
                    hits += T.lexicon_id(T.generate(mb, tb, layer, None, head + p, a.gen), lex)[0] == c
                    n += 1
            arms[arm] = {"hits": hits, "n": n, "acc": round(hits / n, 4),
                         "context_tokens": round(toks / len(concepts), 1) if context else 0,
                         "bytes": round(sum(len(context[c].encode()) for c in concepts) / len(concepts)) if context else 0}

        run("none", None)
        run("words", ctx["words"])
        run("sentences", ctx["sentences"])
        msgs = [m for m in room["messages"] if m["dst"] == dst]
        for arm, key in (("latent", "hits"), ("direct", "direct_hits")):
            h, n = sum(m[key] for m in msgs), sum(m["n"] for m in msgs)
            arms[arm] = {"hits": h, "n": n, "acc": round(h / n, 4), "context_tokens": 0, "bytes": round(env_bytes) if arm == "latent" else 0}
        out["per_receiver"][dst] = arms
        for arm, v in arms.items():
            t = totals.setdefault(arm, [0, 0, 0, 0])
            t[0] += v["hits"]; t[1] += v["n"]; t[2] += v["context_tokens"]; t[3] += v["bytes"]
        del mb
        print(dst, {k: v["acc"] for k, v in arms.items()}, flush=True)
    out["all_receivers"] = {arm: {"acc": round(t[0] / t[1], 4), "n": t[1], "context_tokens": round(t[2] / len(R.MODELS), 1),
                                  "bytes": round(t[3] / len(R.MODELS))} for arm, t in totals.items()}
    text = json.dumps(out, indent=1)
    if a.out:
        Path(a.out).write_text(text)
    print(json.dumps(out["all_receivers"], indent=1))


if __name__ == "__main__":
    main()
