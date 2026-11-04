"""Does more fit data lift the cross-family map? Hold out the 16 original topics (never fitted) and 5 hand-written test concepts,
fit on random subsets of the other topics at growing size, and score top-1 retrieval of each held-out topic's mapped thought
among the true receiver-space directions (21-way, chance 4.8%). Mean over seeds. Run on a GPU (tools/telepathy_scale_modal.py)."""
from __future__ import annotations

import json
import random

import torch

from . import telepathy as T
from .telepathy_data import FIT_TOPICS, NEUTRAL, TEST_CONCEPTS
from .telepathy_data_xl import FIT_TOPICS_XL


def ridge(xa, xb, lam):
    ma, mb = xa.mean(0), xb.mean(0)
    a, b = xa - ma, xb - mb
    g = a @ a.T
    return a.T @ torch.linalg.solve(g + lam * g.diagonal().mean() * torch.eye(a.shape[0], device=a.device), b)


def run(ma, ta, mb, tb, frac=0.45, sizes=(20, 40, 80, 150), seeds=(0, 1, 2), lams=(0.03, 0.1, 0.3), cache=None):
    la, lb = T.layer_of(ma, frac), T.layer_of(mb, frac)
    held = list(FIT_TOPICS)
    pool = [t for t in FIT_TOPICS_XL if t not in FIT_TOPICS]
    tests = list(TEST_CONCEPTS)
    texts, owner = [], []
    for t in pool:
        texts += FIT_TOPICS_XL[t]; owner += [t] * len(FIT_TOPICS_XL[t])
    for t in held:
        texts += FIT_TOPICS_XL[t]; owner += ["held:" + t] * len(FIT_TOPICS_XL[t])
    for c in tests:
        texts += TEST_CONCEPTS[c]["sentences"]; owner += ["test:" + c] * len(TEST_CONCEPTS[c]["sentences"])
    allt = texts + NEUTRAL
    XA, XB = T.sentence_states(ma, ta, allt, la), T.sentence_states(mb, tb, allt, lb)
    n = len(texts)
    na, nb = XA[n:].mean(0), XB[n:].mean(0)
    idx = {}
    for i, o in enumerate(owner):
        idx.setdefault(o, []).append(i)
    keys = ["held:" + t for t in held] + ["test:" + c for c in tests]
    thought = torch.stack([XA[idx[k]].mean(0) - na for k in keys])
    direct = torch.stack([XB[idx[k]].mean(0) - nb for k in keys])
    out = []
    for size in sizes:
        for lam in lams:
            hits, cos = [], []
            for seed in seeds:
                r = random.Random(seed)
                topics = r.sample(pool, min(size, len(pool)))
                rows = [i for t in topics for i in idx[t]]
                W = ridge(XA[rows], XB[rows], lam)
                pred = thought @ W
                sim = torch.nn.functional.cosine_similarity(pred[:, None, :], direct[None, :, :], dim=-1)
                hits.append((sim.argmax(1) == torch.arange(len(keys), device=sim.device)).float().mean().item())
                cos.append(sim.diagonal().mean().item())
            out.append({"topics": min(size, len(pool)), "lam": lam, "top1_21way": round(sum(hits) / len(hits), 4),
                        "mean_cos": round(sum(cos) / len(cos), 4)})
    return out


if __name__ == "__main__":
    print(json.dumps(run.__doc__))
