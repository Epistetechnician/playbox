"""Merging and summarizing E3 shard reports (pure functions; scoring models are in experiments/e3_score.py)."""
from __future__ import annotations

import statistics

from .stats import bootstrap_ci, percentile_against


def merge_shards(shards: list[dict]) -> dict:
    """Combine shard reports of one run. They must agree on the setup and on the deterministic `clean` arm, and no two
    may have produced the same non-clean arm."""
    first = shards[0]
    keys = ("coef", "tokens", "site", "layers", "prompts", "fit_chunks")
    for s in shards[1:]:
        for k in keys:
            if s[k] != first[k]:
                raise ValueError(f"shards disagree on {k}")
        if s["info"]["fit"] != first["info"]["fit"] or s["arms"]["clean"] != first["arms"]["clean"]:
            raise ValueError("shards disagree on the fitted map or on the clean arm: the run is not deterministic")
    arms: dict[str, dict] = {}
    for s in shards:
        for name, a in s["arms"].items():
            if name in arms and name != "clean":
                raise ValueError(f"two shards ran arm {name!r}")
            arms.setdefault(name, a)
    merged = {k: first[k] for k in keys + ("info", "norm_ref")}
    merged["arms"] = arms
    return merged


def summarize(effects: dict[str, list[float]], kl: dict[str, float]) -> dict:
    """`effects[arm]` is the per-prompt score of the arm minus the clean score for the same prompt.

    Reports each arm's mean with a bootstrap interval, the two null populations, and where `direct` and `mapped` sit in
    each. `retained` is mapped / direct on the mean effect; `mapped_minus_direct` is a paired interval over prompts.
    """
    out: dict = {"arms": {}, "populations": {}}
    for name, xs in effects.items():
        m, lo, hi = bootstrap_ci(xs, seed=1)
        out["arms"][name] = {"mean": m, "low": lo, "high": hi, "kl": kl.get(name, 0.0)}
    for pop, prefix in (("mapped_null", "mapped_null"), ("random_null", "random_null")):
        names = sorted(n for n in effects if n.startswith(prefix))
        if len(names) < 2:
            continue
        vals = [out["arms"][n]["mean"] for n in names]
        mean, sd = statistics.fmean(vals), statistics.stdev(vals)
        row = {"n": len(vals), "mean": mean, "sd": sd, "min": min(vals), "max": max(vals),
               "mean_kl": statistics.fmean(out["arms"][n]["kl"] for n in names)}
        for who in ("direct", "mapped"):
            if who in out["arms"]:
                v = out["arms"][who]["mean"]
                row[f"{who}_percentile"] = percentile_against(v, vals)
                row[f"{who}_z"] = (v - mean) / sd if sd > 0 else None
        out["populations"][pop] = row
    if "direct" in effects and "mapped" in effects:
        d, m = out["arms"]["direct"]["mean"], out["arms"]["mapped"]["mean"]
        out["retained"] = m / d if abs(d) > 1e-9 else None
        pm, lo, hi = bootstrap_ci([a - b for a, b in zip(effects["mapped"], effects["direct"])], seed=2)
        out["mapped_minus_direct"] = {"mean": pm, "low": lo, "high": hi}
    return out
