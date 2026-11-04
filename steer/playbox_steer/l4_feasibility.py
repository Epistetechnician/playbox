"""L4 feasibility: can a linear map from SmolLM2-135M's per-layer K/V to Qwen3-1.7B-Base's per-layer K/V give the receiver a
usable context cache?  (docs/l4-scope.md)

For every receiver token we take the sender token whose character span holds the receiver token's midpoint (SwaT's alignment,
eq. 12, without the interpolation), stack the sender's pre-RoPE keys and values of ALL layers into one feature vector, and fit
one ridge map to all receiver layers' pre-RoPE keys and values at once. The predicted receiver cache gets receiver RoPE at
integer positions and is handed to the frozen receiver, which then scores a continuation it never saw the context of.

Metric: retention = (L_none - L_x) / (L_none - L_own), the share of the receiver's own gain from its real context cache that
condition x recovers. L_none: receiver sees the continuation only. L_own: receiver's own cache of the context.
Controls: another item's real cache (is it just 'a plausible cache'?) and the mean cache (is it just 'a cache-shaped thing?').
A second test asks whether the cache carries the CONCEPT: 21-way retrieval of a topic's continuation by likelihood gain.

Run on a GPU: tools/l4_feasibility_modal.py.
"""
from __future__ import annotations

import bisect
import json
import random
import time

import torch
import torch.nn.functional as F
from transformers import AutoModelForCausalLM, AutoTokenizer, DynamicCache

from .telepathy_data import FIT_TOPICS, TEST_CONCEPTS
from .telepathy_data_xl import FIT_TOPICS_XL

DEV = "cuda" if torch.cuda.is_available() else "cpu"


# ---- models and cache capture ----------------------------------------------------------------------------------------

def load(path: str):
    tok = AutoTokenizer.from_pretrained(path)
    model = AutoModelForCausalLM.from_pretrained(path, dtype=torch.float32).eval().to(DEV)
    return model, tok


class Grab:
    """Pre-RoPE keys (after qk-norm where the model has it) and values of every layer, one row per token."""

    def __init__(self, model):
        self.k, self.v, self.hooks = {}, {}, []
        for i, layer in enumerate(model.model.layers):
            att = layer.self_attn
            km = att.k_norm if hasattr(att, "k_norm") else att.k_proj
            self.hooks.append(km.register_forward_hook(lambda _m, _i, out, i=i: self.k.__setitem__(i, out.detach().reshape(out.shape[1], -1))))
            self.hooks.append(att.v_proj.register_forward_hook(lambda _m, _i, out, i=i: self.v.__setitem__(i, out.detach().reshape(out.shape[1], -1))))

    def stack(self) -> tuple[torch.Tensor, torch.Tensor]:
        n = len(self.k)
        return torch.stack([self.k[i] for i in range(n)]), torch.stack([self.v[i] for i in range(n)])  # [L, s, h*d]

    def close(self):
        for h in self.hooks:
            h.remove()


@torch.no_grad()
def kv_of(model, grab: Grab, ids: torch.Tensor):
    model(input_ids=ids[None].to(DEV), use_cache=False)
    return grab.stack()


# ---- alignment by character spans --------------------------------------------------------------------------------------

def enc(tok, text: str):
    e = tok(text, add_special_tokens=False, return_offsets_mapping=True)
    return torch.tensor(e["input_ids"]), e["offset_mapping"]


def align(sender_offs, recv_offs) -> list[int]:
    """For each receiver token: the last sender token that starts at or before the receiver token's character midpoint."""
    starts = [a for a, _ in sender_offs]
    out = []
    for a, b in recv_offs:
        i = bisect.bisect_right(starts, (a + b) / 2) - 1
        out.append(max(0, min(i, len(starts) - 1)))
    return out


# ---- data ------------------------------------------------------------------------------------------------------------

def wiki_paragraphs(split: str) -> list[str]:
    from datasets import load_dataset
    ds = load_dataset("wikitext", "wikitext-2-raw-v1", split=split)
    return [t.strip() for t in ds["text"] if len(t.strip()) > 700 and not t.strip().startswith("=")]


def cut(text: str, n: int) -> tuple[str, str]:
    i = text.rfind(" ", 0, n)
    return text[:i], text[i + 1:]


def fit_chunks(n_wiki: int, n_templ: int, seed: int = 0) -> list[str]:
    r = random.Random(seed)
    paras = wiki_paragraphs("train")
    r.shuffle(paras)
    chunks = [cut(p, 600)[0] for p in paras[:n_wiki]]
    pool = [t for t in FIT_TOPICS_XL if t not in FIT_TOPICS]  # the 16 original topics are held out for testing
    for _ in range(n_templ):
        t = r.choice(pool)
        chunks.append(" ".join(r.sample(FIT_TOPICS_XL[t], 5)))
    r.shuffle(chunks)
    return chunks


@torch.no_grad()
def paired_rows(sm, st, rm, rt, chunks: list[str]):
    gs, gr = Grab(sm), Grab(rm)
    X, Y = [], []
    try:
        for c in chunks:
            sid, soff = enc(st, c)
            rid, roff = enc(rt, c)
            if len(sid) < 8 or len(rid) < 8:
                continue
            sk, sv = kv_of(sm, gs, sid)
            rk, rv = kv_of(rm, gr, rid)
            idx = torch.tensor(align(soff, roff), device=sk.device)
            L = sk.shape[0]
            x = torch.cat([sk[:, idx].permute(1, 0, 2).reshape(len(rid), -1), sv[:, idx].permute(1, 0, 2).reshape(len(rid), -1)], 1)
            y = torch.cat([rk.permute(1, 0, 2).reshape(len(rid), -1), rv.permute(1, 0, 2).reshape(len(rid), -1)], 1)
            X.append(x.half().cpu()); Y.append(y.half().cpu())
    finally:
        gs.close(); gr.close()
    return torch.cat(X), torch.cat(Y)


# ---- ridge ----------------------------------------------------------------------------------------------------------

class LinearMap:
    """Standardised-input ridge, all outputs at once, solved through one eigendecomposition so several lambdas are cheap."""

    def __init__(self, X: torch.Tensor, Y: torch.Tensor, chunk: int = 8192):
        n = X.shape[0]
        self.mx = torch.zeros(X.shape[1], device=DEV); sq = torch.zeros_like(self.mx)
        self.my = torch.zeros(Y.shape[1], device=DEV)
        for i in range(0, n, chunk):
            x = X[i:i + chunk].to(DEV).float()
            self.mx += x.sum(0); sq += (x * x).sum(0); self.my += Y[i:i + chunk].to(DEV).float().sum(0)
        self.mx /= n; self.my /= n
        self.sx = (sq / n - self.mx ** 2).clamp_min(1e-12).sqrt()
        G = torch.zeros(X.shape[1], X.shape[1], device=DEV)
        C = torch.zeros(X.shape[1], Y.shape[1], device=DEV)
        for i in range(0, n, chunk):
            z = (X[i:i + chunk].to(DEV).float() - self.mx) / self.sx
            G += z.T @ z
            C += z.T @ (Y[i:i + chunk].to(DEV).float() - self.my)
        self.e, self.V = torch.linalg.eigh(G / n)
        self.VtC = self.V.T @ (C / n)
        del G, C

    def weights(self, lam: float, rank: int | None = None) -> torch.Tensor:
        e, V, VtC = self.e, self.V, self.VtC
        if rank:
            e, V, VtC = e[-rank:], V[:, -rank:], VtC[-rank:]
        return V @ (VtC / (e + lam * self.e.mean())[:, None])

    def predict(self, X: torch.Tensor, W: torch.Tensor) -> torch.Tensor:
        return ((X.to(DEV).float() - self.mx) / self.sx) @ W + self.my


def r2(fit: "LinearMap", X: torch.Tensor, Y: torch.Tensor, W: torch.Tensor, chunk: int = 1024) -> float:
    """Held-out R^2 over all output dims, accumulated in chunks (the full prediction does not fit next to the models)."""
    sse = sst = 0.0
    ym = Y.float().mean(0).to(DEV)
    for i in range(0, len(X), chunk):
        y = Y[i:i + chunk].to(DEV).float()
        sse += float(((fit.predict(X[i:i + chunk], W) - y) ** 2).sum()); sst += float(((y - ym) ** 2).sum())
    return 1 - sse / sst


# ---- building a receiver cache -----------------------------------------------------------------------------------------

def rope_k(model, k: torch.Tensor, n_heads: int, head_dim: int) -> torch.Tensor:
    """k: [L, s, h*d] pre-RoPE -> [L, h, s, d] with the receiver's RoPE at positions 0..s-1."""
    from transformers.models.qwen3.modeling_qwen3 import apply_rotary_pos_emb
    L, s, _ = k.shape
    kk = k.reshape(L, s, n_heads, head_dim).permute(0, 2, 1, 3)
    pos = torch.arange(s, device=k.device)[None]
    cos, sin = model.model.rotary_emb(kk, pos)
    out = []
    for l in range(L):
        _, kr = apply_rotary_pos_emb(kk[l:l + 1], kk[l:l + 1], cos, sin)
        out.append(kr[0])
    return torch.stack(out)


def make_cache(model, k_pre: torch.Tensor, v: torch.Tensor) -> DynamicCache:
    cfg = model.config
    h, d = cfg.num_key_value_heads, getattr(cfg, "head_dim", cfg.hidden_size // cfg.num_attention_heads)
    kr = rope_k(model, k_pre, h, d)
    vv = v.reshape(v.shape[0], v.shape[1], h, d).permute(0, 2, 1, 3)
    c = DynamicCache()
    for l in range(kr.shape[0]):
        c.update(kr[l][None].contiguous(), vv[l][None].contiguous(), l)
    return c


def split_kv(y: torch.Tensor, n_layers: int):
    half = y.shape[-1] // 2
    return (y[:, :half].reshape(-1, n_layers, half // n_layers).permute(1, 0, 2),
            y[:, half:].reshape(-1, n_layers, half // n_layers).permute(1, 0, 2))


@torch.no_grad()
def nll(model, cont_ids: torch.Tensor, cache=None) -> float:
    """Mean NLL of cont_ids[1:] given cont_ids[:-1] (and the cache if any); the first token is only an input."""
    out = model(input_ids=cont_ids[None].to(DEV), past_key_values=cache, use_cache=cache is not None)
    return float(F.cross_entropy(out.logits[0, :-1].float(), cont_ids[1:].to(DEV)))


# ---- the experiment --------------------------------------------------------------------------------------------------

def run(sender: str, receiver: str, n_wiki=400, n_templ=400, n_test=150, lams=(0.01, 0.1, 1.0), ranks=(None, 512, 128), log=print):
    t0 = time.time()
    sm, st = load(sender); rm, rt = load(receiver)
    nl = rm.config.num_hidden_layers
    chunks = fit_chunks(n_wiki, n_templ)
    nval = max(20, len(chunks) // 10)
    Xv, Yv = paired_rows(sm, st, rm, rt, chunks[:nval])
    Xv, Yv = Xv[:4000], Yv[:4000]
    X, Y = paired_rows(sm, st, rm, rt, chunks[nval:])
    log(f"fit rows {len(X)}, val rows {len(Xv)}, dims {X.shape[1]}->{Y.shape[1]}  [{time.time() - t0:.0f}s]")
    n_fit = len(X)
    fit = LinearMap(X, Y)
    del X, Y
    log(f"eigh done [{time.time() - t0:.0f}s]")

    # choose lambda and rank by held-out reconstruction R^2 (not by the downstream metric)
    sweep, best = [], None
    for rank in ranks:
        for lam in lams:
            W = fit.weights(lam, rank)
            s = r2(fit, Xv, Yv, W)
            sweep.append({"rank": rank, "lam": lam, "val_r2": round(s, 4)})
            log(f"  rank={rank} lam={lam} val R2={s:.4f}")
            if best is None or s > best[0]:
                best = (s, lam, rank)
            del W
    _, lam, rank = best
    W = fit.weights(lam, rank)
    half = Yv.shape[1] // 2
    Xv, Yv = Xv[:3000], Yv[:3000]
    pv = fit.predict(Xv, W)
    kdim = half // nl
    yv = Yv.to(DEV).float()
    r2_k = 1 - float(((pv[:, :half] - yv[:, :half]) ** 2).sum() / ((yv[:, :half] - yv[:, :half].mean(0)) ** 2).sum())
    r2_v = 1 - float(((pv[:, half:] - yv[:, half:]) ** 2).sum() / ((yv[:, half:] - yv[:, half:].mean(0)) ** 2).sum())
    # per-layer-head RMS calibration of the predictions (SwaT does this), estimated on the validation rows
    hd = getattr(rm.config, "head_dim", rm.config.hidden_size // rm.config.num_attention_heads)
    scale = (yv.reshape(-1, 2, nl, kdim // hd, hd).std((0, 4)) / pv.reshape(-1, 2, nl, kdim // hd, hd).std((0, 4)).clamp_min(1e-6))
    mean_cache = fit.my.reshape(2, nl, kdim)
    out = {"chosen": {"lam": lam, "rank": rank}, "sweep": sweep, "val_r2_keys": round(r2_k, 4), "val_r2_values": round(r2_v, 4)}

    sg, rg = Grab(sm), Grab(rm)

    def own(text):
        ids, _ = enc(rt, text)
        k, v = kv_of(rm, rg, ids)
        return ids, k, v

    def mapped(text, calib=False):
        sid, soff = enc(st, text)
        rid, roff = enc(rt, text)
        sk, sv = kv_of(sm, sg, sid)
        idx = torch.tensor(align(soff, roff), device=sk.device)
        x = torch.cat([sk[:, idx].permute(1, 0, 2).reshape(len(rid), -1), sv[:, idx].permute(1, 0, 2).reshape(len(rid), -1)], 1)
        y = fit.predict(x, W)
        if calib:
            yy = y.reshape(-1, 2, nl, kdim // hd, hd)
            mu = fit.my.reshape(1, 2, nl, kdim // hd, hd)
            y = (mu + (yy - mu) * scale[None, :, :, :, None]).reshape(y.shape)
        return split_kv(y, nl)

    def conds(ctx_text, cont_ids, other_text=None):
        r = {"none": nll(rm, cont_ids)}
        ids, k, v = own(ctx_text)
        r["own"] = nll(rm, cont_ids, make_cache(rm, k, v))
        if other_text is not None:
            _, k2, v2 = own(other_text)
            r["other_own"] = nll(rm, cont_ids, make_cache(rm, k2, v2))
        s = k.shape[1]
        r["mean"] = nll(rm, cont_ids, make_cache(rm, mean_cache[0][:, None].expand(-1, s, -1).contiguous(), mean_cache[1][:, None].expand(-1, s, -1).contiguous()))
        mk, mv = mapped(ctx_text)
        r["mapped"] = nll(rm, cont_ids, make_cache(rm, mk, mv))
        mk, mv = mapped(ctx_text, calib=True)
        r["mapped_rms"] = nll(rm, cont_ids, make_cache(rm, mk, mv))
        return r

    # sanity: the cache rebuilt from pre-RoPE K/V must match the receiver's own cache
    paras = wiki_paragraphs("validation")
    ctx0, cont0 = cut(paras[0], 450)
    ids0, k0, v0 = own(ctx0)
    cid0, _ = enc(rt, " " + cont0[:250])
    with torch.no_grad():
        true = rm(input_ids=ids0[None].to(DEV), use_cache=True).past_key_values
        a = nll(rm, cid0, true)
    out["sanity"] = {"true_cache_nll": round(a, 4), "rebuilt_cache_nll": round(nll(rm, cid0, make_cache(rm, k0, v0)), 4)}
    log("sanity", out["sanity"])

    def agg(rows):
        res = {}
        base = [r["none"] for r in rows]; ow = [r["own"] for r in rows]
        gain = sum(base) - sum(ow)
        for c in rows[0]:
            xs = [r[c] for r in rows]
            res[c] = {"nll": round(sum(xs) / len(xs), 4)}
            if c not in ("none", "own"):
                res[c]["retention"] = round((sum(base) - sum(xs)) / gain, 4)
                # item-level bootstrap interval on the retention ratio
                rr = random.Random(0); bs = []
                for _ in range(500):
                    ii = [rr.randrange(len(rows)) for _ in rows]
                    g = sum(base[i] - ow[i] for i in ii)
                    bs.append(sum(base[i] - rows[i][c] for i in ii) / g)
                bs.sort(); res[c]["ci95"] = [round(bs[12], 4), round(bs[487], 4)]
        return res

    # A) natural text (wikitext validation)
    rows = []
    for i in range(min(n_test, len(paras) - 1)):
        ctx, rest = cut(paras[i], 450)
        cont = rest[:250]
        other = cut(paras[(i + 7) % len(paras)], 450)[0]
        cid, _ = enc(rt, " " + cont)
        if len(cid) >= 12:
            rows.append(conds(ctx, cid, other))
    out["wikitext"] = {"n": len(rows), **agg(rows)}
    log("wikitext", out["wikitext"], f"[{time.time() - t0:.0f}s]")

    # B) templated held-out topics (the style of our telepathy fit text)
    r = random.Random(5); rows = []
    names = list(FIT_TOPICS)
    for t in names:
        for _ in range(3):
            s = r.sample(FIT_TOPICS[t], 14)
            other = " ".join(r.sample(FIT_TOPICS[r.choice([n for n in names if n != t])], 12))
            cid, _ = enc(rt, " " + " ".join(s[12:]))
            rows.append(conds(" ".join(s[:12]), cid, other))
    out["templated_heldout"] = {"n": len(rows), **agg(rows)}
    log("templated", out["templated_heldout"], f"[{time.time() - t0:.0f}s]")

    # C) does the cache carry the concept? 21-way retrieval by likelihood gain
    ctxs, conts = {}, {}
    for t in names:
        s = FIT_TOPICS[t]; ctxs[t] = " ".join(s[:12]); conts[t] = " " + " ".join(s[30:33])
    for t, c in TEST_CONCEPTS.items():
        s = c["sentences"]; ctxs["test:" + t] = " ".join(s[:5]); conts["test:" + t] = " " + " ".join(s[5:])
    keys = list(ctxs)
    cids = {k: enc(rt, conts[k])[0] for k in keys}
    none = {k: nll(rm, cids[k]) for k in keys}
    caches = {"own": {}, "mapped": {}, "mapped_rms": {}}
    for k in keys:
        _, a, b = own(ctxs[k]); caches["own"][k] = (a, b)
        caches["mapped"][k] = mapped(ctxs[k]); caches["mapped_rms"][k] = mapped(ctxs[k], calib=True)
    ret = {}
    for cond, store in caches.items():
        hit = 0
        for c in keys:
            gains = {i: none[c] - nll(rm, cids[c], make_cache(rm, *store[i])) for i in keys}
            hit += int(max(gains, key=gains.get) == c)
        ret[cond] = round(hit / len(keys), 4)
    out["concept_retrieval_21way"] = {"chance": round(1 / len(keys), 4), **ret}
    log("retrieval", out["concept_retrieval_21way"], f"[{time.time() - t0:.0f}s]")
    out["seconds"] = round(time.time() - t0)
    out["fit_rows"] = n_fit
    return out


if __name__ == "__main__":
    print(json.dumps(run.__doc__))
