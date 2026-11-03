"""Offline sweep for the cross-family channel: no text generation, no agents, one process (about 8 GB).

For every (layer in A, layer in B, map type): hold out one topic, fit the map on the others' sentences, build the
held-out topic's thought vector from 6 random sentences in A (as an agent would), map it, and ask which topic's
direct B-side vector it is closest to by cosine. Chance is 1/16. Also reports the mean cosine to the right topic.
"""
from __future__ import annotations

import json
import random
import subprocess
import sys
from pathlib import Path

import torch

from . import telepathy as T
from .telepathy_data import FIT_TOPICS, NEUTRAL, TEST_CONCEPTS
from .telepathy_agent import path


def free_gb() -> float:
    out = subprocess.run(["vm_stat"], capture_output=True, text=True).stdout
    page = int(out.split("page size of ")[1].split()[0])
    return sum(int(l.split(":")[1].strip(" .")) for l in out.splitlines()
               if l.split(":")[0] in ("Pages free", "Pages inactive", "Pages speculative")) * page / 2**30


def states_multi(model, tok, texts, layers):
    """Mean over non-first tokens at several layers in one pass: {layer: [N, d]}."""
    cur, rows = {}, {l: [] for l in layers}
    hs = [model.model.layers[l].register_forward_hook(
        lambda _m, _i, out, l=l: cur.__setitem__(l, (out[0] if isinstance(out, tuple) else out).detach())) for l in layers]
    try:
        for t in texts:
            model(input_ids=tok(t, return_tensors="pt").input_ids[:, :64], use_cache=False)
            for l in layers:
                x = cur[l][0]
                rows[l].append(x[1:].mean(0) if x.shape[0] > 1 else x[0])
    finally:
        for h in hs:
            h.remove()
    return {l: torch.stack(v) for l, v in rows.items()}


def ridge(xa, xb, lam):
    ma, mb = xa.mean(0), xb.mean(0)
    a, b = xa - ma, xb - mb
    g = a @ a.T
    w = a.T @ torch.linalg.solve(g + lam * g.diagonal().mean() * torch.eye(a.shape[0]), b)
    return w


def main(src: str, dst: str, fracs=(0.3, 0.45, 0.6, 0.75), lams=(0.01, 0.1, 1.0), draws=5, out=None):
    if free_gb() < 12:
        raise SystemExit(f"need about 12 GB free, have {free_gb():.1f}")
    torch.manual_seed(0)
    ma, ta = T.load(path(src)); mb, tb = T.load(path(dst))
    topics = list(FIT_TOPICS)
    texts, owner = [], []
    for t in topics:
        texts += FIT_TOPICS[t]; owner += [t] * len(FIT_TOPICS[t])
    texts_all = texts + NEUTRAL
    la = {f: T.layer_of(ma, f) for f in fracs}; lb = {f: T.layer_of(mb, f) for f in fracs}
    SA = states_multi(ma, ta, texts_all, sorted(set(la.values()))); SB = states_multi(mb, tb, texts_all, sorted(set(lb.values())))
    n = len(texts)
    owner_t = [topics.index(o) for o in owner]
    res = []
    rng = random.Random(0)
    for fa in fracs:
        for fb in fracs:
            XA, XB = SA[la[fa]], SB[lb[fb]]
            neu_a, neu_b = XA[n:].mean(0), XB[n:].mean(0)
            direct = torch.stack([XB[[i for i in range(n) if owner_t[i] == k]].mean(0) - neu_b for k in range(len(topics))])
            for lam in lams:
                hit, cos = 0, 0.0
                trials = 0
                for k in range(len(topics)):
                    keep = [i for i in range(n) if owner_t[i] != k]
                    W = ridge(XA[keep], XB[keep], lam)
                    idx = [i for i in range(n) if owner_t[i] == k]
                    for _ in range(draws):
                        pick = rng.sample(idx, 6)
                        v = (XA[pick].mean(0) - neu_a) @ W
                        sims = torch.nn.functional.cosine_similarity(v[None], direct, dim=-1)
                        hit += int(sims.argmax() == k); cos += float(sims[k]); trials += 1
                res.append({"frac_a": fa, "frac_b": fb, "layer_a": la[fa], "layer_b": lb[fb], "lam": lam,
                            "retrieval": hit / trials, "mean_cos": cos / trials})
                print(json.dumps(res[-1]), flush=True)
    res.sort(key=lambda r: (-r["retrieval"], -r["mean_cos"]))
    if out:
        Path(out).write_text(json.dumps(res, indent=1))
    print("best:", json.dumps(res[:5], indent=1))
    return res


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], out=sys.argv[3] if len(sys.argv) > 3 else None)


def learning_curve(src: str, dst: str, frac=0.45, lam=0.1, draws=8, sizes=(3, 6, 9, 12, 15), reps=6, out=None):
    """Retrieval of a held-out topic as the map is fitted on fewer topics: does more (and more varied) text help?"""
    if free_gb() < 12:
        raise SystemExit(f"need about 12 GB free, have {free_gb():.1f}")
    ma, ta = T.load(path(src)); mb, tb = T.load(path(dst))
    la, lb = T.layer_of(ma, frac), T.layer_of(mb, frac)
    topics = list(FIT_TOPICS)
    texts, owner = [], []
    for t in topics:
        texts += FIT_TOPICS[t]; owner += [topics.index(t)] * len(FIT_TOPICS[t])
    XA = states_multi(ma, ta, texts + NEUTRAL, [la])[la]; XB = states_multi(mb, tb, texts + NEUTRAL, [lb])[lb]
    n = len(texts)
    neu_a, neu_b = XA[n:].mean(0), XB[n:].mean(0)
    direct = torch.stack([XB[[i for i in range(n) if owner[i] == k]].mean(0) - neu_b for k in range(len(topics))])
    rng = random.Random(1)
    rows = []
    for size in sizes:
        hit = trials = 0
        for _ in range(reps):
            for k in range(len(topics)):
                others = [t for t in range(len(topics)) if t != k]
                fit_topics = rng.sample(others, min(size, len(others)))
                keep = [i for i in range(n) if owner[i] in fit_topics]
                W = ridge(XA[keep], XB[keep], lam)
                idx = [i for i in range(n) if owner[i] == k]
                for _ in range(draws // 4 or 1):
                    v = (XA[rng.sample(idx, 6)].mean(0) - neu_a) @ W
                    hit += int(torch.nn.functional.cosine_similarity(v[None], direct, dim=-1).argmax() == k); trials += 1
        rows.append({"fit_topics": size, "retrieval": round(hit / trials, 3), "n": trials})
        print(json.dumps(rows[-1]), flush=True)
    if out:
        Path(out).write_text(json.dumps(rows, indent=1))
    return rows


def compare_fit(src: str, dst: str, frac=0.45, lams=(0.03, 0.1, 0.3), out=None):
    """Old 16-topic fit vs the 56-topic fit, on (a) the 5 hand-written test concepts and (b) leave-one-topic-out."""
    from .telepathy_data_big import FIT_TOPICS_BIG
    if free_gb() < 12:
        raise SystemExit(f"need about 12 GB free, have {free_gb():.1f}")
    ma, ta = T.load(path(src)); mb, tb = T.load(path(dst))
    la, lb = T.layer_of(ma, frac), T.layer_of(mb, frac)
    topics = list(FIT_TOPICS_BIG)
    texts, owner = [], []
    for k, t in enumerate(topics):
        texts += FIT_TOPICS_BIG[t]; owner += [k] * len(FIT_TOPICS_BIG[t])
    tests = list(TEST_CONCEPTS)
    test_texts = [s for c in tests for s in TEST_CONCEPTS[c]["sentences"]]
    allt = texts + NEUTRAL + test_texts
    XA = states_multi(ma, ta, allt, [la])[la]; XB = states_multi(mb, tb, allt, [lb])[lb]
    n = len(texts)
    neu_a, neu_b = XA[n:n + len(NEUTRAL)].mean(0), XB[n:n + len(NEUTRAL)].mean(0)
    TA, TB = XA[n + len(NEUTRAL):], XB[n + len(NEUTRAL):]
    tdirect = torch.stack([TB[i * 8:(i + 1) * 8].mean(0) - neu_b for i in range(len(tests))])
    tthought = [(TA[i * 8:(i + 1) * 8].mean(0) - neu_a) for i in range(len(tests))]
    old = len(FIT_TOPICS)
    rows = []
    for name, ntop in (("16 topics", old), ("56 topics", len(topics))):
        keep = [i for i in range(n) if owner[i] < ntop]
        for lam in lams:
            W = ridge(XA[keep], XB[keep], lam)
            hit = sum(int(torch.nn.functional.cosine_similarity((tthought[i] @ W)[None], tdirect, dim=-1).argmax() == i) for i in range(len(tests)))
            cos = sum(float(torch.nn.functional.cosine_similarity((tthought[i] @ W)[None], tdirect[i:i + 1], dim=-1)) for i in range(len(tests))) / len(tests)
            # leave-one-topic-out over the OLD 16 topics, so both fits are scored on the same held-out topics
            direct = torch.stack([XB[[i for i in range(n) if owner[i] == k]].mean(0) - neu_b for k in range(old)])
            rng, h2, tr = random.Random(2), 0, 0
            for k in range(old):
                kk = [i for i in range(n) if owner[i] < ntop and owner[i] != k]
                Wk = ridge(XA[kk], XB[kk], lam)
                idx = [i for i in range(n) if owner[i] == k]
                for _ in range(5):
                    v = (XA[rng.sample(idx, 6)].mean(0) - neu_a) @ Wk
                    h2 += int(torch.nn.functional.cosine_similarity(v[None], direct, dim=-1).argmax() == k); tr += 1
            rows.append({"fit": name, "lam": lam, "test5_retrieval": hit / len(tests), "test5_mean_cos": round(cos, 3), "loto16_retrieval": round(h2 / tr, 3)})
            print(json.dumps(rows[-1]), flush=True)
    if out:
        Path(out).write_text(json.dumps(rows, indent=1))
    return rows
