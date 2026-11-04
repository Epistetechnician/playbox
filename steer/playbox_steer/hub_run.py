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
from .anchors_extra import EXTRA_TOPICS, extra_anchors
from .telepathy_data import FIT_TOPICS, NEUTRAL, PROMPTS, TEST_CONCEPTS


def _anchor_split(seed: int = 0, held: int = 150) -> tuple[list[str], list[str]]:
    texts = [s for topic in FIT_TOPICS.values() for s in topic]
    random.Random(seed).shuffle(texts)
    return texts[held:], texts[:held]


def _sender_pass(path: str, frac: float, train: list[str], heldout: list[str], concepts: list[str], bank: dict | None = None) -> dict:
    m, tok = T.load(path)
    layer = T.layer_of(m, frac)
    neutral = T.sentence_states(m, tok, NEUTRAL, layer)
    out = {
        "layer": layer,
        "train": T.sentence_states(m, tok, train, layer),
        "held": T.sentence_states(m, tok, heldout, layer),
        "norm": T.mean_norm(m, tok, NEUTRAL + PROMPTS, layer),
        "thought": {c: T.thought(m, tok, layer, (bank or TEST_CONCEPTS)[c]["sentences"], neutral) for c in concepts},
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
                coef: float, gen: int, bank: dict | None = None, direct: bool = False) -> dict:
    bank = bank or TEST_CONCEPTS
    lex = {c: set(bank[c]["lexicon"]) for c in concepts}
    res: dict = {"hubs": {n: {"k": h.k, "models": list(h.spokes)} for n, h in hubs.items()}, "pairs": {}, "chance": 1 / len(concepts)}
    if direct:
        res["direct"] = {}
    for dst, di in info.items():
        mb, tb = T.load(models[dst])
        lb = di["layer"]
        if direct:  # the receiver's own direction for each concept: what it could do having read the sentences itself
            neutral = T.sentence_states(mb, tb, NEUTRAL, lb)
            per = {}
            for c in concepts:
                d = T.thought(mb, tb, lb, bank[c]["sentences"], neutral)
                d = d / d.norm() * coef * di["norm"]
                per[c] = sum(T.lexicon_id(T.generate(mb, tb, lb, d, p, gen), lex)[0] == c for p in prompts) / len(prompts)
            res["direct"][dst] = {"acc": round(sum(per.values()) / len(per), 4), "by_concept": per}
        for name, hub in hubs.items():
            joiner = name[5:] if name.startswith("join_") else None
            for src, si in info.items():
                if src == dst or (joiner and joiner not in (src, dst)):
                    continue
                hits = n = 0
                per = {}
                for c in concepts:
                    raw, scale = H.quantize(hub.encode(src, si["thought"][c]))
                    v = hub.decode(dst, H.dequantize(raw, scale))
                    v = v / v.norm() * coef * di["norm"]
                    h_c = sum(T.lexicon_id(T.generate(mb, tb, lb, v, p, gen), lex)[0] == c for p in prompts)
                    hits += h_c
                    n += len(prompts)
                    per[c] = h_c / len(prompts)
                cos = torch.nn.functional.cosine_similarity(
                    hub.send(src, dst, si["held"] - si["held"].mean(0)), di["held"] - di["held"].mean(0), dim=-1).mean().item()
                res["pairs"].setdefault(f"{src}->{dst}", {})[name] = {"acc": round(hits / n, 4), "heldout_cos": round(cos, 4),
                                                                      "by_concept": per}
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


_TEMPLATE_WORDS = {"the", "a", "near", "this", "morning", "people", "often", "say", "is", "but", "anyway", "everyone", "noticed",
                   "how", "beside", "story", "about", "and", "that", "all", "day"}
_STOP = _TEMPLATE_WORDS | {"an", "of", "in", "on", "to", "was", "were", "his", "her", "he", "she", "as", "by", "for", "with",
                           "from", "under", "across", "toward", "over", "into", "it", "its", "their", "they", "them", "at",
                           "before", "after", "then", "than", "while", "had", "has", "have", "be", "been", "or", "so", "up", "another",
                           "around", "both", "along", "against", "began", "came", "until", "during", "every", "each",
                           "some", "many", "other", "more", "above", "below", "past", "like"}


def _content_words(sentence: str) -> list[str]:
    return [w for w in "".join(c if c.isalpha() else " " for c in sentence.lower()).split() if w not in _STOP]


def card_text(weights: dict[str, float], topics: dict[str, float], n_words: int = 6, n_topics: int = 3) -> str:
    """The concept card a text-only agent reads: the nearest anchor topics and words, with no sentence from the sender."""
    top_t = [t for t, _ in sorted(topics.items(), key=lambda kv: -kv[1])[:n_topics]]
    top_w = [w for w, _ in sorted(weights.items(), key=lambda kv: -kv[1])[:n_words]]
    return f"[hub card] nearest topics: {', '.join(top_t)}. nearest words: {', '.join(top_w)}."


def text_baseline(sentences: list[str], n_words: int = 6, corpus: list[str] | None = None) -> str:
    """Same word budget as the card, but written by the sender from the sentences it read: the words that are most frequent
    in them and rare elsewhere (tf-idf against `corpus`, the anchors and every test sentence), the way a person would
    pick key words."""
    import math

    corpus = corpus or []
    df: dict[str, int] = {}
    for sent in corpus:
        for w in set(_content_words(sent)):
            df[w] = df.get(w, 0) + 1
    counts: dict[str, int] = {}
    for sent in sentences:
        for w in _content_words(sent):
            counts[w] = counts.get(w, 0) + 1
    score = {w: c * math.log((len(corpus) + 1) / (df.get(w, 0) + 1)) for w, c in counts.items()}
    order = sorted(score, key=lambda w: (-score[w], -len(w), w))  # longer words are usually the nouns
    return f"[text summary] key words: {', '.join(order[:n_words])}."


def cards(models: dict[str, str], extra: int = 16, k: int = 256, rank_frac: float = 0.6, frac: float = 0.45,
          near: int = 24, n_words: int = 6, n_topics: int = 3, n_null: int = 25, concepts: list[str] | None = None) -> dict:
    """Closed-model arm, sender side (docs/latent-channels.md 6e). For every sender model and test concept: the hub vector
    (what crosses the wire, int8) is decoded against the anchor corpus into a concept card, with no text from the sender.
    Also: the same concept as a word-budget-matched text summary, the plain sentences, and cards from random vectors."""
    torch.set_grad_enabled(False)
    concepts = concepts or list(TEST_CONCEPTS)
    train, held = _anchor_split()
    more = extra_anchors(extra, set(train) | set(held))
    anchors = train + more
    topic_of = {sent: t for t, sents in {**FIT_TOPICS, **EXTRA_TOPICS}.items() for sent in sents}
    info = {n: _sender_pass(p, frac, anchors, held, concepts) for n, p in models.items()}
    hub = H.fit_hub({n: i["train"] for n, i in info.items()}, H.anchor_digest(anchors), k=k, r=round(rank_frac * len(anchors)))
    gn = torch.nn.functional.normalize(hub.g, dim=-1)

    def card_for(h: torch.Tensor) -> dict:
        sims = gn @ torch.nn.functional.normalize(h, dim=-1)
        idx = sims.topk(min(near, len(anchors))).indices.tolist()
        words: dict[str, float] = {}
        topics: dict[str, float] = {}
        for j in idx:
            w = float(sims[j])
            topics[topic_of[anchors[j]]] = topics.get(topic_of[anchors[j]], 0.0) + w
            for word in set(_content_words(anchors[j])):
                words[word] = words.get(word, 0.0) + w
        return {"text": card_text(words, topics, n_words, n_topics), "top_anchor_cos": round(float(sims[idx[0]]), 4)}

    corpus = anchors + [t for c in TEST_CONCEPTS.values() for t in c["sentences"]]
    items = []
    for src, si in info.items():
        for c in concepts:
            raw, scale = H.quantize(hub.encode(src, si["thought"][c]))
            sents = TEST_CONCEPTS[c]["sentences"]
            items.append({"src": src, "concept": c, "card": card_for(H.dequantize(raw, scale))["text"],
                          "text": text_baseline(sents, n_words, corpus), "sentences": " ".join(sents), "wire_bytes": len(raw)})
    g = torch.Generator().manual_seed(0)
    nulls = []
    names = list(info)
    for i in range(n_null):
        src = names[i % len(names)]
        r = torch.randn(info[src]["hidden"], generator=g)
        raw, scale = H.quantize(hub.encode(src, r / r.norm() * info[src]["thought"][concepts[0]].norm()))
        nulls.append({"src": src, "card": card_for(H.dequantize(raw, scale))["text"]})
    return {"hub": {"k": hub.k, "anchors": len(anchors), "models": list(info)}, "items": items, "nulls": nulls,
            "concepts": concepts, "chance": 1 / len(concepts)}


def join_chain(models: dict[str, str], base: tuple[str, ...] | None = None, orders: tuple[tuple[str, ...], ...] | None = None,
               extra: int = 16, k: int = 256, rank_frac: float = 0.6, frac: float = 0.45, coef: float = 0.5, gen: int = 24,
               concepts: list[str] | None = None, prompts: list[str] | None = None) -> dict:
    """E8 follow-up: does a hub that grows one model at a time drift from one fitted on everyone?

    `all`: the hub fitted on every model at once. `chain_<a>-<b>-...`: fitted on the `base` models only, then each model of
    the order joins in turn with its own adapters; nobody is refitted, so an earlier spoke never changes when a later one
    joins, and what can differ from `all` is the shared coordinates (built from the base models alone) and so how well the
    joiners fit them. Same anchors (426 + `extra` topics), whitening rank `rank_frac * n`, strength and scoring as 6d."""
    torch.set_grad_enabled(False)
    concepts = concepts or list(TEST_CONCEPTS)
    prompts = prompts or PROMPTS
    names = list(models)
    base = base or tuple(names[:2])
    rest = [n for n in names if n not in base]
    orders = orders or (tuple(rest), tuple(reversed(rest)))
    train, held = _anchor_split()
    anchors = train + extra_anchors(extra, set(train) | set(held))
    info = {n: _sender_pass(p, frac, anchors, held, concepts) for n, p in models.items()}
    digest = H.anchor_digest(anchors)
    r = round(rank_frac * len(anchors))
    states = {n: i["train"] for n, i in info.items()}
    hubs: dict[str, H.Hub] = {"all": H.fit_hub(states, digest, k=k, r=r)}
    for order in orders:
        hub = H.fit_hub({n: states[n] for n in base}, digest, k=k, r=r)
        for n in order:
            hub.add_spoke(n, states[n])
        hubs["chain_" + "-".join(order)] = hub
    res = _score_hubs(models, info, hubs, concepts, prompts, coef, gen)
    res["base"] = list(base)
    res["orders"] = [list(o) for o in orders]
    return res


def natural_eval(models: dict[str, str], texts: list[str], n_nat: int = 1000, k: int = 256, rank_frac: float = 0.6,
                 frac: float = 0.45, coef: float = 0.5, gen: int = 24, n_prompts: int = 4,
                 concepts: list[str] | None = None, template_topics: int = 16) -> dict:
    """E8 follow-up (docs/latent-channels.md 6k): natural-text anchors and twenty concepts.

    Anchors: the 1000 template sentences (`tem`), `n_nat` and `2*n_nat` natural sentences from `texts` with every sentence
    containing a concept lexicon word removed (`nat<n>`), and both together (`mix`). Concepts: the original five plus fifteen
    natural-text ones (`concepts_natural`), chance 5%. Held-out cosine is measured on natural sentences that no hub saw.
    Hubs are named `tem`, `nat<n>`, `nat<2n>`, `mix`; k and the whitening rank rule are the same for all."""
    from .concepts_natural import ALL_CONCEPTS, natural_anchors

    torch.set_grad_enabled(False)
    concepts = concepts or list(ALL_CONCEPTS)
    prompts = PROMPTS[:n_prompts]
    train, held_t = _anchor_split()
    tem = train + extra_anchors(template_topics, set(train) | set(held_t))
    nat = natural_anchors(texts, 2 * n_nat + 150)
    held, nat = nat[2 * n_nat:], nat[: 2 * n_nat]
    anchors = tem + nat
    info = {n: _sender_pass(p, frac, anchors, held, concepts, ALL_CONCEPTS) for n, p in models.items()}
    nt = len(tem)
    sets = {"tem": list(range(nt)), f"nat{n_nat}": list(range(nt, nt + n_nat)), f"nat{2 * n_nat}": list(range(nt, nt + 2 * n_nat)),
            "mix": list(range(nt + n_nat))}
    hubs: dict[str, H.Hub] = {}
    for name, idx in sets.items():
        hubs[name] = H.fit_hub({m: i["train"][idx] for m, i in info.items()}, H.anchor_digest([anchors[j] for j in idx]), k=k,
                               r=round(rank_frac * len(idx)))
    res = _score_hubs(models, info, hubs, concepts, prompts, coef, gen, ALL_CONCEPTS, direct=True)
    res["anchor_sizes"] = {name: len(idx) for name, idx in sets.items()}
    res["heldout_natural"] = len(held)
    res["concepts"] = concepts
    return res
