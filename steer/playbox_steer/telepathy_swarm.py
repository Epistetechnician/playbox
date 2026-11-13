"""Swarm primitives for the telepathy channel: several sender agents, one summarizer in another model family.

The idea comes from latent agent swarms (SwarmEvolve, arXiv 2611.08483): agents that never write text hand their reasoning to
a summarizer, and a novelty filter keeps the agents whose hidden states differ most. Here each sender agent reads its own
sentences, its thought is mapped into the summarizer's space by the fitted ridge map, and the summarizer receives ONE vector.

    assign_agents   n agents over k true concepts, one concept dominant and every other concept held by a single agent
    novelty_select  the k agents farthest apart in cosine distance (greedy farthest-first), the paper's novelty filter
    aggregate       sum the unit directions, renormalise, scale to the channel's strength
    score_trial     which concepts the summarizer's continuations name: top-k by lexicon hits, recall and precision
    paired_sign_p   exact two-sided sign test on paired per-trial scores
Pure torch and Python: nothing here loads a model, so it is tested without one (tests/test_telepathy_swarm.py)."""
from __future__ import annotations

import math
import random

import torch

ARMS = ("none", "text_words", "latent_one", "latent_all", "latent_random", "latent_novelty", "latent_novelty_x", "direct", "null")


def assign_agents(concepts: list[str], k: int, n: int, rng: random.Random) -> tuple[list[str], list[str]]:
    """k distinct true concepts and one concept per agent (n >= k). true[0] is the dominant concept, held by n-k+1 agents; every
    other true concept is held by exactly one agent. Agent order is shuffled."""
    if n < k or k > len(concepts):
        raise ValueError("need n >= k and k <= number of concepts")
    true = rng.sample(concepts, k)
    agents = [true[0]] * (n - k + 1) + true[1:]
    rng.shuffle(agents)
    return true, agents


def unit(v: torch.Tensor) -> torch.Tensor:
    return v / v.norm().clamp_min(1e-12)


def novelty_select(vecs: torch.Tensor, k: int) -> list[int]:
    """Greedy farthest-first on cosine distance. Start with the agent whose mean cosine distance to the others is largest, then
    repeatedly add the agent whose smallest distance to the chosen ones is largest. Ties go to the lower index."""
    n = vecs.shape[0]
    if not 1 <= k <= n:
        raise ValueError("need 1 <= k <= number of vectors")
    u = torch.stack([unit(v.float()) for v in vecs])
    dist = 1 - u @ u.T
    first = int(torch.argmax(dist.sum(1) / max(n - 1, 1)))
    chosen = [first]
    while len(chosen) < k:
        rest = [i for i in range(n) if i not in chosen]
        best = max(rest, key=lambda i: (float(dist[i, chosen].min()), -i))
        chosen.append(best)
    return chosen


def aggregate(vecs: torch.Tensor, coef: float, norm_b: float, scale: float = 1.0) -> torch.Tensor:
    """Sum of unit directions, renormalised to the strength one vector gets on the channel (coef * norm_b), times scale."""
    s = torch.stack([unit(v.float()) for v in vecs]).sum(0)
    return unit(s) * coef * scale * norm_b


def _words(text: str) -> list[str]:
    return [w.strip(".,;:!?\"'()").lower() for w in text.split()]


def score_trial(texts: list[str], lex: dict[str, set[str]], true: list[str]) -> dict:
    """Total lexicon hits per concept over all continuations; the predicted set is the top-k concepts with at least one hit
    (ties by concept order). recall = share of the true concepts predicted; precision = share of predictions that are true."""
    hits = {c: 0 for c in lex}
    for t in texts:
        for w in _words(t):
            for c, words in lex.items():
                if w in words:
                    hits[c] += 1
    order = list(lex)
    ranked = sorted(order, key=lambda c: (-hits[c], order.index(c)))
    pred = [c for c in ranked if hits[c] > 0][: len(true)]
    got = [c for c in pred if c in true]
    return {"hits": hits, "pred": pred, "recall": len(got) / len(true), "full": len(got) == len(true),
            "precision": len(got) / len(pred) if pred else 0.0, "found": got}


def paired_sign_p(a: list[float], b: list[float]) -> tuple[int, int, float]:
    """(a wins, b wins, exact two-sided p) over pairs that differ."""
    wa = sum(x > y for x, y in zip(a, b))
    wb = sum(y > x for x, y in zip(a, b))
    n = wa + wb
    if n == 0:
        return 0, 0, 1.0
    k = min(wa, wb)
    p = 2 * sum(math.comb(n, i) for i in range(0, k + 1)) / 2**n
    return wa, wb, min(1.0, p)
