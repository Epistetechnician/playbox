"""Settling test (B) of docs/l4-scope.md section 9: on templated held-out topics, does the mapped cache beat an unrelated real cache by 20 points?

Section 10/11 measured (B) on 48 items (16 topics x 3 draws) and got 19.1 points against 20, intervals that nearly touch. This evaluates many more
draws per topic with several draw seeds, and reports the difference the way it should be reported: the items of one topic are not independent
(the 16 topics are the real sample), so the interval is a cluster bootstrap over topics, and the retention difference is computed on the same items
for both conditions. Extra control, `mapped_wrong`: the mapper fed a different topic's sender cache and scored on this item's continuation; mapped
minus mapped_wrong is topic specificity with format priming held fixed (the real unrelated cache is a different kind of object, a receiver-made cache).

Retention = (L_none - L_x) / (L_none - L_own), pooled over items (sum of losses), as in section 3."""
from __future__ import annotations

import json
import random
import time

import torch

from . import l4_swat as S
from .l4_eval import caches, nll_with
from .telepathy_data import FIT_TOPICS

CONDS = ("none", "own", "other_own", "mapped", "mapped_wrong")


def retention(rows: list[dict], cond: str) -> float:
    gain = sum(r["none"] - r["own"] for r in rows)
    return sum(r["none"] - r[cond] for r in rows) / gain


def diff(rows: list[dict], a: str, b: str) -> float:
    return retention(rows, a) - retention(rows, b)


def cluster_bootstrap(rows: list[dict], a: str, b: str, n_boot: int = 2000, seed: int = 0) -> dict:
    """Point estimate and 95% interval of retention(a) - retention(b), resampling whole topics with replacement."""
    by = {}
    for r in rows:
        by.setdefault(r["topic"], []).append(r)
    topics = sorted(by)
    rng = random.Random(seed)
    vals = []
    for _ in range(n_boot):
        pick = [rng.choice(topics) for _ in topics]
        vals.append(diff([x for t in pick for x in by[t]], a, b))
    vals.sort()
    return {"point": round(diff(rows, a, b), 4), "ci95": [round(vals[int(0.025 * n_boot)], 4), round(vals[int(0.975 * n_boot) - 1], 4)],
            "n_items": len(rows), "n_topics": len(topics)}


def per_topic(rows: list[dict], a: str, b: str) -> dict:
    by = {}
    for r in rows:
        by.setdefault(r["topic"], []).append(r)
    return {t: round(diff(v, a, b), 3) for t, v in sorted(by.items())}


@torch.no_grad()
def run(ckpt: str, seeds=(5, 6, 7), draws: int = 15, log=print) -> dict:
    t0 = time.time()
    su = S.load_mapper(ckpt, dtype=torch.float32)
    su.mapper.eval()
    names = list(FIT_TOPICS)
    rows = []
    for seed in seeds:
        r = random.Random(seed)
        for t in names:
            for d in range(draws):
                s = r.sample(FIT_TOPICS[t], 14)
                o = r.choice([n for n in names if n != t])
                other_ctx = " ".join(r.sample(FIT_TOPICS[o], 12))
                ctx, cont = " ".join(s[:12]), " " + " ".join(s[12:])
                bt, own, mapped = caches(su, ctx, cont)
                _, other, mapped_other = caches(su, other_ctx, cont)
                rows.append({"seed": seed, "topic": t, "none": nll_with(su, bt, None), "own": nll_with(su, bt, own),
                             "other_own": nll_with(su, bt, other), "mapped": nll_with(su, bt, mapped), "mapped_wrong": nll_with(su, bt, mapped_other)})
        log(f"seed {seed}: {len(rows)} items [{time.time() - t0:.0f}s]", flush=True)
    out = {"ckpt": ckpt, "draw_seeds": list(seeds), "draws_per_topic": draws, "topics": len(names), "n_items": len(rows)}
    out["retention_pooled"] = {c: round(retention(rows, c), 4) for c in CONDS if c not in ("none", "own")}
    out["B_mapped_minus_unrelated_real"] = cluster_bootstrap(rows, "mapped", "other_own")
    out["specificity_mapped_minus_mapped_wrong"] = cluster_bootstrap(rows, "mapped", "mapped_wrong")
    out["by_draw_seed"] = {str(sd): {"B": round(diff([r for r in rows if r["seed"] == sd], "mapped", "other_own"), 4)} for sd in seeds}
    out["B_by_topic"] = per_topic(rows, "mapped", "other_own")
    out["seconds"] = round(time.time() - t0)
    return out
