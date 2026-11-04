"""Held-out evaluation of a trained L4 mapper, with the protocol of docs/l4-scope.md section 3 (so it compares with the linear
map): receiver continuation NLL with no context, with its own real context cache, with an unrelated real cache, with the mean
cache, and with the mapped cache; retention = (L_none - L_x) / (L_none - L_own). fp32 models, batch size 1.
"""
from __future__ import annotations

import json
import random
import time

import torch

from . import l4_swat as S
from .telepathy_data import FIT_TOPICS, TEST_CONCEPTS

DEV = S.DEV


def cut(text: str, n: int):
    i = text.rfind(" ", 0, n)
    return text[:i], text[i + 1:]


def one(su: S.Setup, ctx: str, cont: str):
    p = S.Pool([(ctx, cont)], su.st, su.rt)
    assert len(p) == 1, (ctx[:40], cont[:40])
    return p.batch([0], su.pad_s, su.pad_r)


@torch.no_grad()
def caches(su: S.Setup, ctx: str, cont: str):
    bt = one(su, ctx, cont)
    sk, sv = su.sender_cache(bt)
    rk, rv = su.receiver_cache(bt)
    pk, pv = su.predict(bt, sk, sv)
    return bt, (rk, rv), (pk, pv)


@torch.no_grad()
def nll_with(su, bt, kv, cont_bt=None):
    cb = cont_bt or bt
    cache = None if kv is None else S.rope_cache(su.rm, kv[0], kv[1], su.rd)
    mask = torch.zeros(1, 0, dtype=torch.bool, device=DEV) if kv is None else torch.ones(1, kv[0].shape[2], dtype=torch.bool, device=DEV)
    return float(S.cont_nll(su.rm, cache, mask, cb["c_ids"], cb["c_mask"]))


def agg(rows, seed=0):
    out = {}
    base = [r["none"] for r in rows]
    own = [r["own"] for r in rows]
    gain = sum(base) - sum(own)
    for c in rows[0]:
        xs = [r[c] for r in rows]
        out[c] = {"nll": round(sum(xs) / len(xs), 4)}
        if c not in ("none", "own"):
            out[c]["retention"] = round((sum(base) - sum(xs)) / gain, 4)
            rr, bs = random.Random(seed), []
            for _ in range(1000):
                ii = [rr.randrange(len(rows)) for _ in rows]
                g = sum(base[i] - own[i] for i in ii)
                bs.append(sum(base[i] - rows[i][c] for i in ii) / g)
            bs.sort()
            out[c]["ci95"] = [round(bs[25], 4), round(bs[974], 4)]
    return out


def run(ckpt: str, sender=None, receiver=None, n_wiki=150, log=print):
    t0 = time.time()
    su = S.load_mapper(ckpt, sender, receiver, dtype=torch.float32)
    su.mapper.eval()
    ck = torch.load(ckpt, map_location="cpu")
    out = {"ckpt_hist": ck.get("hist", [])[-3:], "sender": ck["sender"], "receiver": ck["receiver"]}
    # mean cache from a few training-style paragraphs
    cal = S.build_items(S.wiki_texts("wikitext-2-raw-v1", "train"), 64, 3)
    mk = mv = 0
    with torch.no_grad():
        n = 0
        for c, t in cal:
            _, (rk, rv), _ = caches(su, c, t)
            mk = mk + rk.mean(2, keepdim=True); mv = mv + rv.mean(2, keepdim=True); n += 1
    mean_kv = (mk / n, mv / n)

    def conds(ctx, cont, other_ctx):
        bt, own, mapped = caches(su, ctx, cont)
        _, other, _ = caches(su, other_ctx, cont)
        N = own[0].shape[2]
        r = {"none": nll_with(su, bt, None), "own": nll_with(su, bt, own), "other_own": nll_with(su, bt, other),
             "mean": nll_with(su, bt, (mean_kv[0].expand(-1, -1, N, -1), mean_kv[1].expand(-1, -1, N, -1))),
             "mapped": nll_with(su, bt, mapped)}
        return r

    paras = S.wiki_texts("wikitext-2-raw-v1", "validation", min_chars=700)
    rows = []
    for i in range(min(n_wiki, len(paras) - 1)):
        ctx, rest = cut(paras[i], 450)
        other = cut(paras[(i + 7) % len(paras)], 450)[0]
        rows.append(conds(ctx, " " + rest[:250], other))
    out["wikitext"] = {"n": len(rows), **agg(rows)}
    log("wikitext", json.dumps(out["wikitext"]), f"[{time.time() - t0:.0f}s]")

    r = random.Random(5)
    rows, names = [], list(FIT_TOPICS)
    for t in names:
        for _ in range(3):
            s = r.sample(FIT_TOPICS[t], 14)
            other = " ".join(r.sample(FIT_TOPICS[r.choice([n for n in names if n != t])], 12))
            rows.append(conds(" ".join(s[:12]), " " + " ".join(s[12:]), other))
    out["templated_heldout"] = {"n": len(rows), **agg(rows)}
    log("templated", json.dumps(out["templated_heldout"]), f"[{time.time() - t0:.0f}s]")

    ctxs, conts = {}, {}
    for t in names:
        s = FIT_TOPICS[t]; ctxs[t] = " ".join(s[:12]); conts[t] = " " + " ".join(s[30:33])
    for t, c in TEST_CONCEPTS.items():
        s = c["sentences"]; ctxs["test:" + t] = " ".join(s[:5]); conts["test:" + t] = " " + " ".join(s[5:])
    keys = list(ctxs)
    pre = {k: caches(su, ctxs[k], conts[k]) for k in keys}  # (bt, own, mapped) with each topic's own continuation
    none = {k: nll_with(su, pre[k][0], None) for k in keys}
    ret = {}
    for cond, pick in (("own", 1), ("mapped", 2)):
        hit = 0
        for c in keys:
            gains = {i: none[c] - nll_with(su, pre[c][0], pre[i][pick]) for i in keys}
            hit += int(max(gains, key=gains.get) == c)
        ret[cond] = round(hit / len(keys), 4)
    out["concept_retrieval_21way"] = {"chance": round(1 / len(keys), 4), **ret}
    log("retrieval", json.dumps(out["concept_retrieval_21way"]))
    out["seconds"] = round(time.time() - t0)
    return out
