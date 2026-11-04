"""E8: one hub for N model families. Does a thought sent through the hub steer a receiver as well as a map fitted for
that exact pair, and as well as the receiver's own direction?

    python -m playbox_steer.hub_run --model smollm2=/path/a --model qwen3=/path/b --model gpt2=/path/c --out e8.json

Each model is loaded one at a time (as a sender, then as a receiver), so memory is one model. For every ordered pair
(src -> dst) and every test concept, arms at the same strength (in the receiver's mean token norm):
    hub     src thought -> hub -> dst, quantized to int8 on the way (what would cross the wire)
    pair    src thought through a ridge map fitted for this exact pair (telepathy.py), the baseline the hub must match
    direct  the receiver's own diffmean for the concept (upper bound)
    null    random vectors in src space through the hub
Decoding is the lexicon decoder from telepathy.py. Test concepts are never in the anchors. Ceiling:
local_internal_exploratory.
"""
from __future__ import annotations

import argparse
import json
import random
from pathlib import Path

import torch

from . import hub as H
from . import telepathy as T
from .anchors_extra import extra_anchors
from .telepathy_data import FIT_TOPICS, NEUTRAL, PROMPTS, TEST_CONCEPTS


def _anchor_split(seed: int = 0, held: int = 150) -> tuple[list[str], list[str]]:
    texts = [s for topic in FIT_TOPICS.values() for s in topic]
    random.Random(seed).shuffle(texts)
    return texts[held:], texts[:held]


def _sender_pass(path: str, frac: float, train: list[str], heldout: list[str], concepts: list[str]) -> dict:
    m, tok = T.load(path)
    layer = T.layer_of(m, frac)
    neutral = T.sentence_states(m, tok, NEUTRAL, layer)
    out = {
        "layer": layer,
        "train": T.sentence_states(m, tok, train, layer),
        "held": T.sentence_states(m, tok, heldout, layer),
        "norm": T.mean_norm(m, tok, NEUTRAL + PROMPTS, layer),
        "thought": {c: T.thought(m, tok, layer, TEST_CONCEPTS[c]["sentences"], neutral) for c in concepts},
        "neutral_mean": neutral.mean(0),
        "hidden": m.config.hidden_size,
    }
    del m
    return out


def run(models: dict[str, str], frac: float = 0.45, k: int = 64, coefs: tuple[float, ...] = (0.5,), n_null: int = 6,
        gen: int = 24, concepts: list[str] | None = None, prompts: list[str] | None = None) -> dict:
    torch.set_grad_enabled(False)
    concepts = concepts or list(TEST_CONCEPTS)
    prompts = prompts or PROMPTS
    train, held = _anchor_split()
    info = {n: _sender_pass(p, frac, train, held, concepts) for n, p in models.items()}
    hub = H.fit_hub({n: i["train"] for n, i in info.items()}, H.anchor_digest(train), k=k)
    lex = {c: set(TEST_CONCEPTS[c]["lexicon"]) for c in concepts}
    g = torch.Generator().manual_seed(0)
    res: dict = {"hub": {"id": hub.hub_id, "k": hub.k, "anchors": len(train), "models": {n: i["hidden"] for n, i in info.items()},
                         "wire_bytes_per_thought": hub.k, "pair_maps_avoided": len(models) * (len(models) - 1) - 2 * len(models)},
                 "heldout_cosine": {}, "pairs": {}, "chance": 1 / len(concepts)}

    for dst, di in info.items():
        mb, tb = T.load(models[dst])
        lb = di["layer"]
        cache: dict[tuple, str] = {}

        def decode(vec, key):
            outs = []
            for p in prompts:
                if (key, p) not in cache:
                    cache[(key, p)] = T.generate(mb, tb, lb, vec, p, gen)
                outs.append(cache[(key, p)])
            return outs

        def score(vec, key, concept):
            hits = [T.lexicon_id(t, lex)[0] == concept for t in decode(vec, key)]
            return sum(hits), len(hits)

        direct = {c: T.thought(mb, tb, lb, TEST_CONCEPTS[c]["sentences"], T.sentence_states(mb, tb, NEUTRAL, lb))
                  for c in concepts}
        for src, si in info.items():
            if src == dst:
                continue
            key = f"{src}->{dst}"
            pair = T.Ridge(si["train"], di["train"], 0.1)
            ho_hub = torch.nn.functional.cosine_similarity(
                hub.send(src, dst, si["held"] - si["held"].mean(0)), di["held"] - di["held"].mean(0), dim=-1).mean().item()
            ho_pair = torch.nn.functional.cosine_similarity(
                pair.direction(si["held"] - si["held"].mean(0)), di["held"] - di["held"].mean(0), dim=-1).mean().item()
            res["heldout_cosine"][key] = {"hub": round(ho_hub, 4), "pair": round(ho_pair, 4)}
            for coef in coefs:
                tally = {a: [0, 0] for a in ("hub", "pair", "direct", "null")}

                def add(arm, vec, tag, concept):
                    v = vec / vec.norm() * coef * di["norm"]
                    c, n = score(v, (arm, tag, coef), concept)
                    tally[arm][0] += c
                    tally[arm][1] += n

                for c in concepts:
                    d = si["thought"][c]
                    h = hub.encode(src, d)
                    raw, scale = H.quantize(h)  # what crosses the wire
                    add("hub", hub.decode(dst, H.dequantize(raw, scale)), (src, c), c)
                    add("pair", pair.direction(d), (src, c), c)
                    add("direct", direct[c], c, c)
                for i in range(n_null):
                    r = torch.randn(si["hidden"], generator=g)
                    r = r / r.norm() * (si["thought"][concepts[0]].norm())
                    add("null", hub.send(src, dst, r), (src, "null", i), concepts[i % len(concepts)])
                res["pairs"].setdefault(key, {})[str(coef)] = {a: round(c / n, 4) for a, (c, n) in tally.items()}
        del mb
    return res


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", action="append", required=True, metavar="NAME=PATH")
    ap.add_argument("--frac", type=float, default=0.45)
    ap.add_argument("--k", type=int, default=64)
    ap.add_argument("--coefs", default="0.5")
    ap.add_argument("--nulls", type=int, default=6)
    ap.add_argument("--gen", type=int, default=24)
    ap.add_argument("--out")
    a = ap.parse_args()
    models = dict(m.split("=", 1) for m in a.model)
    r = run(models, a.frac, a.k, tuple(float(x) for x in a.coefs.split(",")), a.nulls, a.gen)
    text = json.dumps(r, indent=1)
    if a.out:
        Path(a.out).write_text(text)
    print(text)


if __name__ == "__main__":
    main()


def sweep(models: dict[str, str], ks: tuple[int, ...] = (16, 32, 128), join: bool = True, frac: float = 0.45,
          coef: float = 0.5, gen: int = 24, concepts: list[str] | None = None, prompts: list[str] | None = None) -> dict:
    """E8 follow-up, hub arm only: accuracy for other hub sizes k, and for a model that joins a hub fitted on the others.

    `join_<m>`: the hub is fitted on every model but m, then m gets its own adapters against that hub (nobody is refitted).
    Only pairs that involve m are scored for it. Compare with the all-models hub of the main run (same anchors, k=64).
    """
    torch.set_grad_enabled(False)
    concepts = concepts or list(TEST_CONCEPTS)
    prompts = prompts or PROMPTS
    train, held = _anchor_split()
    info = {n: _sender_pass(p, frac, train, held, concepts) for n, p in models.items()}
    digest = H.anchor_digest(train)
    states = {n: i["train"] for n, i in info.items()}
    hubs: dict[str, H.Hub] = {f"k{k}": H.fit_hub(states, digest, k=k) for k in ks}
    if join:
        for m in info:
            hub = H.fit_hub({n: s for n, s in states.items() if n != m}, digest, k=64)
            hub.add_spoke(m, states[m])
            hubs[f"join_{m}"] = hub
    return _score_hubs(models, info, hubs, concepts, prompts, coef, gen)


def _score_hubs(models: dict[str, str], info: dict, hubs: dict[str, H.Hub], concepts: list[str], prompts: list[str],
                coef: float, gen: int) -> dict:
    lex = {c: set(TEST_CONCEPTS[c]["lexicon"]) for c in concepts}
    res: dict = {"hubs": {n: {"k": h.k, "models": list(h.spokes)} for n, h in hubs.items()}, "pairs": {}, "chance": 1 / len(concepts)}
    for dst, di in info.items():
        mb, tb = T.load(models[dst])
        lb = di["layer"]
        for name, hub in hubs.items():
            joiner = name[5:] if name.startswith("join_") else None
            for src, si in info.items():
                if src == dst or (joiner and joiner not in (src, dst)):
                    continue
                hits = n = 0
                for c in concepts:
                    raw, scale = H.quantize(hub.encode(src, si["thought"][c]))
                    v = hub.decode(dst, H.dequantize(raw, scale))
                    v = v / v.norm() * coef * di["norm"]
                    for p in prompts:
                        hits += T.lexicon_id(T.generate(mb, tb, lb, v, p, gen), lex)[0] == c
                        n += 1
                cos = torch.nn.functional.cosine_similarity(
                    hub.send(src, dst, si["held"] - si["held"].mean(0)), di["held"] - di["held"].mean(0), dim=-1).mean().item()
                res["pairs"].setdefault(f"{src}->{dst}", {})[name] = {"acc": round(hits / n, 4), "heldout_cos": round(cos, 4)}
        del mb
    return res


def anchor_sweep(models: dict[str, str], ns: tuple[int, ...] = (100, 200, 300), ks: tuple[int, ...] = (128,),
                 seeds: tuple[int, ...] = (0, 1, 2), rank_frac: float = 0.6, frac: float = 0.45, coef: float = 0.5,
                 gen: int = 24, concepts: list[str] | None = None, prompts: list[str] | None = None) -> dict:
    """E8 follow-up: hub fitted on n random anchors, for each hub size k and each subset seed, plus the full anchor set.

    The whitening rank is `min(256, round(rank_frac * n))`, so it scales with n instead of keeping every direction when
    n is small (the first anchor sweep's confound); for all 426 anchors that is 256, the main run's setting.
    Hubs are named `n<n>_s<seed>_k<k>` and `full_k<k>`. Held-out sentences and concepts are unchanged."""
    torch.set_grad_enabled(False)
    concepts = concepts or list(TEST_CONCEPTS)
    prompts = prompts or PROMPTS
    train, held = _anchor_split()
    info = {n: _sender_pass(p, frac, train, held, concepts) for n, p in models.items()}
    hubs: dict[str, H.Hub] = {}
    for k in ks:
        hubs[f"full_k{k}"] = H.fit_hub({m: i["train"] for m, i in info.items()}, H.anchor_digest(train), k=k,
                                       r=min(256, round(rank_frac * len(train))))
    for n in ns:
        for seed in seeds:
            idx = sorted(random.Random(1000 + seed).sample(range(len(train)), n))
            sub = [train[j] for j in idx]
            for k in ks:
                hubs[f"n{n}_s{seed}_k{k}"] = H.fit_hub({m: i["train"][idx] for m, i in info.items()}, H.anchor_digest(sub),
                                                      k=k, r=min(256, round(rank_frac * n)))
    return _score_hubs(models, info, hubs, concepts, prompts, coef, gen)


def anchor_extend(models: dict[str, str], extra: tuple[int, ...] = (0, 8, 16), ks: tuple[int, ...] = (128, 256),
                  rank_frac: float = 0.6, frac: float = 0.45, coef: float = 0.5, gen: int = 24,
                  concepts: list[str] | None = None, prompts: list[str] | None = None) -> dict:
    """E8 follow-up: more anchors than the 426. The hub is fitted on the 426 plus the sentences of the first `extra`
    topics of `anchors_extra` (36 each), for each hub size k. Whitening rank is `round(rank_frac * n)` with no cap
    (256 at 426, the main run's setting). Held-out sentences and concepts are unchanged. Hubs are named `x<extra>_k<k>`."""
    torch.set_grad_enabled(False)
    concepts = concepts or list(TEST_CONCEPTS)
    prompts = prompts or PROMPTS
    train, held = _anchor_split()
    more = extra_anchors(max(extra), set(train) | set(held))
    info = {n: _sender_pass(p, frac, train + more, held, concepts) for n, p in models.items()}
    hubs: dict[str, H.Hub] = {}
    for x in extra:
        n = len(train) + len(extra_anchors(x, set(train) | set(held)))
        sub = (train + more)[:n]
        for k in ks:
            hubs[f"x{x}_k{k}"] = H.fit_hub({m: i["train"][:n] for m, i in info.items()}, H.anchor_digest(sub), k=k,
                                           r=round(rank_frac * n))
    res = _score_hubs(models, info, hubs, concepts, prompts, coef, gen)
    for name in res["hubs"]:
        res["hubs"][name]["anchors"] = len(train) + len(extra_anchors(int(name[1:].split("_")[0]), set(train) | set(held)))
    return res


def pair_control(models: dict[str, str], extra: tuple[int, ...] = (0, 8, 16), frac: float = 0.45, coef: float = 0.5,
                 gen: int = 24, concepts: list[str] | None = None, prompts: list[str] | None = None) -> dict:
    """Control for `anchor_extend`: the per-pair ridge map (the main run's baseline) fitted on the same extra anchors.

    Same anchors, held-out sentences, concepts, prompts and scoring as the hub arms, so `pair_x<extra>` compares with
    the hub `x<extra>_k<k>`. `pair_x0` is the main run's pair arm again (a check that the setup reproduces it)."""
    torch.set_grad_enabled(False)
    concepts = concepts or list(TEST_CONCEPTS)
    prompts = prompts or PROMPTS
    train, held = _anchor_split()
    more = extra_anchors(max(extra), set(train) | set(held))
    info = {n: _sender_pass(p, frac, train + more, held, concepts) for n, p in models.items()}
    lex = {c: set(TEST_CONCEPTS[c]["lexicon"]) for c in concepts}
    res: dict = {"arms": {f"pair_x{x}": len(train) + len(extra_anchors(x, set(train) | set(held))) for x in extra},
                 "pairs": {}, "chance": 1 / len(concepts)}
    for dst, di in info.items():
        mb, tb = T.load(models[dst])
        lb = di["layer"]
        for src, si in info.items():
            if src == dst:
                continue
            for x in extra:
                n = res["arms"][f"pair_x{x}"]
                pair = T.Ridge(si["train"][:n], di["train"][:n], 0.1)
                hits = tot = 0
                for c in concepts:
                    v = pair.direction(si["thought"][c])
                    v = v / v.norm() * coef * di["norm"]
                    for p in prompts:
                        hits += T.lexicon_id(T.generate(mb, tb, lb, v, p, gen), lex)[0] == c
                        tot += 1
                cos = torch.nn.functional.cosine_similarity(
                    pair.direction(si["held"] - si["held"].mean(0)), di["held"] - di["held"].mean(0), dim=-1).mean().item()
                res["pairs"].setdefault(f"{src}->{dst}", {})[f"pair_x{x}"] = {"acc": round(hits / tot, 4), "heldout_cos": round(cos, 4)}
        del mb
    return res
