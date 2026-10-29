from __future__ import annotations

import math
import random
import statistics


def bootstrap_ci(xs: list[float], n: int = 2000, seed: int = 0, level: float = 0.95) -> tuple[float, float, float]:
    """(mean, low, high) of the mean, by percentile bootstrap over items."""
    if not xs:
        return (math.nan, math.nan, math.nan)
    rng = random.Random(seed)
    means = sorted(statistics.fmean(rng.choices(xs, k=len(xs))) for _ in range(n))
    return (statistics.fmean(xs), means[int((1 - level) / 2 * n)], means[min(n - 1, int((1 + level) / 2 * n))])


def percentile_against(value: float, population: list[float]) -> float:
    """Fraction of `population` strictly below `value`."""
    return sum(p < value for p in population) / len(population) if population else math.nan


def ranks(xs: list[float]) -> list[float]:
    """Average ranks (ties share the mean rank)."""
    order = sorted(range(len(xs)), key=lambda i: xs[i])
    out = [0.0] * len(xs)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and xs[order[j + 1]] == xs[order[i]]:
            j += 1
        for k in range(i, j + 1):
            out[order[k]] = (i + j) / 2 + 1
        i = j + 1
    return out


def spearman(a: list[float], b: list[float]) -> float:
    if len(a) != len(b) or len(a) < 3:
        return math.nan
    ra, rb = ranks(a), ranks(b)
    ma, mb = statistics.fmean(ra), statistics.fmean(rb)
    num = sum((x - ma) * (y - mb) for x, y in zip(ra, rb))
    den = math.sqrt(sum((x - ma) ** 2 for x in ra) * sum((y - mb) ** 2 for y in rb))
    return num / den if den else math.nan
