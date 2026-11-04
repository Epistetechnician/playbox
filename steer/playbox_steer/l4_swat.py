"""A cross-family KV-cache mapper in the style of SwaT (arXiv 2610.15095, CC BY 4.0), written from the paper's method section.
Nothing here is copied from the unlicensed `Ar-Kareem/SwaT` repository.

Two frozen models, a sender S and a receiver R of different families. The mapper reads S's pre-RoPE keys and values of every
layer and writes R's keys and values of every layer, one entry per R token, so R can continue a text it never read.

    block b (b = 0..B-1) maps  h_b -> h_{b+1}:   causal self-attention over R-token states (integer RoPE),
                                                 causal cross-attention to a linear projection of S's cache for the layer
                                                 group S_b (RoPE of sender positions from character spans, eq. 12),
                                                 GELU feed-forward, each sublayer with its own LayerNorm and residual.
    output of block b: K and V of the receiver layer group R_b, scaled per layer and head to calibrated receiver statistics.

Layer groups: group b holds layers floor(bL/B) .. floor((b+1)L/B)-1 of each model. Cross-tokenizer handling: sender token i
gets the fractional receiver position  p_i = max(0, j + clip01((m_i - a_j)/(b_j - a_j)) - 1/2)  where m_i is its character
midpoint and j the last receiver token starting at or before m_i; a receiver token j sees a sender token only if the sender
token's character end is at or before the receiver token's end (causal in text).

Stage 1 trains normalised MSE to the receiver's own cache; stage 2 trains the receiver's next-token loss on a continuation
of the document, reading only the mapped cache. Deviations from the paper (ours): output scaling multiplies by the calibrated
RMS instead of re-normalising each head vector; schedules are shorter; stage-2 learning rate is larger (the paper's 3e-6
assumes 24k steps).
"""
from __future__ import annotations

import bisect
import json
import math
import random
import time

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F
from transformers import AutoModelForCausalLM, AutoTokenizer, DynamicCache

DEV = "cuda" if torch.cuda.is_available() else "cpu"


# ---- models and cache capture ----------------------------------------------------------------------------------------

def load(path: str, dtype=torch.bfloat16):
    tok = AutoTokenizer.from_pretrained(path)
    model = AutoModelForCausalLM.from_pretrained(path, dtype=dtype).eval().to(DEV)
    for p in model.parameters():
        p.requires_grad_(False)
    return model, tok


def kv_dims(model) -> dict:
    c = model.config
    hd = getattr(c, "head_dim", None) or c.hidden_size // c.num_attention_heads
    return {"L": c.num_hidden_layers, "H": c.num_key_value_heads, "d": hd, "kd": c.num_key_value_heads * hd,
            "emb": c.hidden_size}


class GrabB:
    """Pre-RoPE keys (after qk-norm where the model has it) and values of every layer: lists of [B, S, H*d]."""

    def __init__(self, model):
        self.k, self.v, self.hooks = {}, {}, []
        for i, layer in enumerate(model.model.layers):
            att = layer.self_attn
            km = att.k_norm if hasattr(att, "k_norm") else att.k_proj
            self.hooks.append(km.register_forward_hook(lambda _m, _i, out, i=i: self.k.__setitem__(i, out.reshape(out.shape[0], out.shape[1], -1))))
            self.hooks.append(att.v_proj.register_forward_hook(lambda _m, _i, out, i=i: self.v.__setitem__(i, out.reshape(out.shape[0], out.shape[1], -1))))

    def stack(self):
        n = len(self.k)
        return torch.stack([self.k[i] for i in range(n)], 1), torch.stack([self.v[i] for i in range(n)], 1)  # [B, L, S, H*d]

    def close(self):
        for h in self.hooks:
            h.remove()


# ---- alignment ---------------------------------------------------------------------------------------------------------

def sender_positions(s_off: np.ndarray, r_off: np.ndarray) -> np.ndarray:
    """eq. 12: fractional receiver-position of every sender token."""
    m = (s_off[:, 0] + s_off[:, 1]) / 2.0
    j = np.searchsorted(r_off[:, 0], m, side="right") - 1
    jc = np.clip(j, 0, len(r_off) - 1)
    a, b = r_off[jc, 0], r_off[jc, 1]
    frac = np.clip((m - a) / np.maximum(b - a, 1), 0, 1)
    return np.where(j < 0, 0.0, np.maximum(0.0, jc + frac - 0.5)).astype(np.float32)


def encode_many(tok, texts: list[str], max_len: int):
    e = tok(texts, add_special_tokens=False, return_offsets_mapping=True, truncation=True, max_length=max_len)
    return e["input_ids"], [np.asarray(o, dtype=np.int64).reshape(-1, 2) for o in e["offset_mapping"]]


# ---- data ------------------------------------------------------------------------------------------------------------

def split_text(text: str, n_ctx: int, n_cont: int):
    i = text.rfind(" ", 0, n_ctx)
    if i < 40:
        return None
    rest = text[i + 1:]
    j = rest.rfind(" ", 0, n_cont)
    if j < 40:
        return None
    return text[:i], rest[:j]


def build_items(split_texts: list[str], n_items: int, seed: int, ctx_chars=(350, 900), cont_chars=(180, 320)):
    r = random.Random(seed)
    out = []
    pool = list(split_texts)
    r.shuffle(pool)
    for t in pool:
        sp = split_text(t, r.randint(*ctx_chars), r.randint(*cont_chars))
        if sp:
            out.append(sp)
        if len(out) >= n_items:
            break
    return out


def wiki_texts(config: str, split: str, min_chars=700) -> list[str]:
    from datasets import load_dataset
    ds = load_dataset("wikitext", config, split=split)
    return [t.strip() for t in ds["text"] if len(t.strip()) > min_chars and not t.strip().startswith("=")]


def templated_items(n: int, seed: int):
    """Topical template text in our telepathy-fit style; the 16 original topics are held out for evaluation."""
    from .telepathy_data import FIT_TOPICS
    from .telepathy_data_xl import FIT_TOPICS_XL
    r = random.Random(seed)
    pool = [t for t in FIT_TOPICS_XL if t not in FIT_TOPICS]
    out = []
    for _ in range(n):
        s = r.sample(FIT_TOPICS_XL[r.choice(pool)], 11)
        out.append((" ".join(s[:8]), " ".join(s[8:])))
    return out


class Pool:
    """Pre-tokenised (context, continuation) pairs for one sender/receiver pair."""

    def __init__(self, items, st, rt, max_ctx_tokens=256, max_cont_tokens=96):
        ctx = [c for c, _ in items]
        cont = [" " + t for _, t in items]
        self.s_ids, s_off = encode_many(st, ctx, 320)
        self.r_ids, r_off = encode_many(rt, ctx, max_ctx_tokens)
        self.c_ids, _ = encode_many(rt, cont, max_cont_tokens)
        self.items = []
        for i in range(len(items)):
            if len(self.r_ids[i]) < 8 or len(self.s_ids[i]) < 8 or len(self.c_ids[i]) < 8:
                continue
            roff = r_off[i]
            soff = s_off[i]
            keep = soff[:, 0] < roff[-1, 1]  # drop sender tokens past a truncated receiver context
            n_s = int(keep.sum())
            if n_s < 4:
                continue
            soff = soff[:n_s]
            self.items.append({
                "s": np.asarray(self.s_ids[i][:n_s]), "r": np.asarray(self.r_ids[i]), "c": np.asarray(self.c_ids[i]),
                "pos": sender_positions(soff, roff), "s_end": soff[:, 1].astype(np.int64), "r_end": roff[:, 1].astype(np.int64)})

    def __len__(self):
        return len(self.items)

    def batch(self, idx: list[int], pad_s: int, pad_r: int):
        its = [self.items[i] for i in idx]
        B = len(its)
        S, N, Q = max(len(x["s"]) for x in its), max(len(x["r"]) for x in its), max(len(x["c"]) for x in its)
        f = lambda shape, v, dt: torch.full(shape, v, dtype=dt)
        s_ids, r_ids, c_ids = f((B, S), pad_s, torch.long), f((B, N), pad_r, torch.long), f((B, Q), pad_r, torch.long)
        s_mask, r_mask, c_mask = f((B, S), 0, torch.bool), f((B, N), 0, torch.bool), f((B, Q), 0, torch.bool)
        pos = f((B, S), 0.0, torch.float32)
        s_end, r_end = f((B, S), 1 << 40, torch.long), f((B, N), -1, torch.long)
        for b, x in enumerate(its):
            ns, nr, nc = len(x["s"]), len(x["r"]), len(x["c"])
            s_ids[b, :ns] = torch.from_numpy(x["s"]); r_ids[b, :nr] = torch.from_numpy(x["r"]); c_ids[b, :nc] = torch.from_numpy(x["c"])
            s_mask[b, :ns] = True; r_mask[b, :nr] = True; c_mask[b, :nc] = True
            pos[b, :ns] = torch.from_numpy(x["pos"]); s_end[b, :ns] = torch.from_numpy(x["s_end"]); r_end[b, :nr] = torch.from_numpy(x["r_end"])
        cross = (s_end[:, None, :] <= r_end[:, :, None]) & s_mask[:, None, :]  # [B, N, S]
        cross[:, :, 0] |= ~cross.any(-1)  # every row sees at least the first sender token (no all-masked softmax)
        return {k: v.to(DEV) for k, v in dict(s_ids=s_ids, r_ids=r_ids, c_ids=c_ids, s_mask=s_mask, r_mask=r_mask, c_mask=c_mask,
                                                pos=pos, cross=cross).items()}


# ---- the mapper ------------------------------------------------------------------------------------------------------

def rope(x: torch.Tensor, pos: torch.Tensor, base: float = 10000.0) -> torch.Tensor:
    """x [B, H, L, dh]; pos [B, L] (floats allowed)."""
    dh = x.shape[-1]
    inv = 1.0 / (base ** (torch.arange(0, dh, 2, device=x.device, dtype=torch.float32) / dh))
    ang = pos[:, None, :, None].float() * inv
    cos, sin = ang.cos().to(x.dtype), ang.sin().to(x.dtype)
    x1, x2 = x[..., : dh // 2], x[..., dh // 2:]
    return torch.cat([x1 * cos - x2 * sin, x1 * sin + x2 * cos], -1)


class Attn(nn.Module):
    def __init__(self, D, H):
        super().__init__()
        self.H, self.q, self.k, self.v, self.o = H, nn.Linear(D, D), nn.Linear(D, D), nn.Linear(D, D), nn.Linear(D, D)

    def forward(self, x, mem, pos_q, pos_k, mask=None, causal=False):
        B, N, D = x.shape
        sh = lambda t, L: t.view(B, L, self.H, D // self.H).transpose(1, 2)
        q, k, v = sh(self.q(x), N), sh(self.k(mem), mem.shape[1]), sh(self.v(mem), mem.shape[1])
        q, k = rope(q, pos_q), rope(k, pos_k)
        y = F.scaled_dot_product_attention(q, k, v, attn_mask=None if causal else mask[:, None], is_causal=causal)
        return self.o(y.transpose(1, 2).reshape(B, N, D))


class Block(nn.Module):
    def __init__(self, D, H, Fd, mem_in, out_dim):
        super().__init__()
        self.ln1, self.ln2, self.ln3, self.ln_o = nn.LayerNorm(D), nn.LayerNorm(D), nn.LayerNorm(D), nn.LayerNorm(D)
        self.self_attn, self.cross, self.mem = Attn(D, H), Attn(D, H), nn.Linear(mem_in, D)
        self.ff = nn.Sequential(nn.Linear(D, Fd), nn.GELU(), nn.Linear(Fd, D))
        self.out = nn.Linear(D, out_dim)

    def forward(self, h, mem_raw, pos_r, pos_s, cross_mask):
        x = self.ln1(h)
        h = h + self.self_attn(x, x, pos_r, pos_r, causal=True)
        m = self.mem(mem_raw)
        h = h + self.cross(self.ln2(h), m, pos_r, pos_s, mask=cross_mask)
        h = h + self.ff(self.ln3(h))
        return h, self.out(self.ln_o(h))


class Mapper(nn.Module):
    def __init__(self, sd: dict, rd: dict, B=7, D=1024, H=16, Fd=4096):
        super().__init__()
        self.sd, self.rd, self.B = sd, rd, B
        grp = lambda L, b: (b * L // B, (b + 1) * L // B)
        self.sg, self.rg = [grp(sd["L"], b) for b in range(B)], [grp(rd["L"], b) for b in range(B)]
        self.w_in = nn.Linear(rd["emb"], D)
        self.blocks = nn.ModuleList(
            Block(D, H, Fd, (self.sg[b][1] - self.sg[b][0]) * 2 * sd["kd"], (self.rg[b][1] - self.rg[b][0]) * 2 * rd["kd"]) for b in range(B))
        self.register_buffer("sigma", torch.ones(rd["L"], 2, rd["H"]))  # calibrated receiver RMS per layer, K/V, head

    def forward(self, r_emb, sk, sv, pos_s, cross):
        """r_emb [B,N,emb]; sk, sv [B, L_S, S, kd]; returns predicted K, V [B, L_R, N, kd_R]."""
        B_, N, _ = r_emb.shape
        h = self.w_in(r_emb)
        pos_r = torch.arange(N, device=h.device)[None].expand(B_, -1).float()
        ks, vs = [], []
        for b, blk in enumerate(self.blocks):
            lo, hi = self.sg[b]
            mem = torch.cat([sk[:, lo:hi], sv[:, lo:hi]], 1).permute(0, 2, 1, 3).reshape(B_, sk.shape[2], -1)
            h, out = blk(h, mem, pos_r, pos_s, cross)
            lo, hi = self.rg[b]
            o = out.view(B_, N, hi - lo, 2, self.rd["H"], self.rd["d"]) * self.sigma[lo:hi][None, None, :, :, :, None].to(out.dtype)
            ks.append(o[:, :, :, 0].reshape(B_, N, hi - lo, -1).permute(0, 2, 1, 3))
            vs.append(o[:, :, :, 1].reshape(B_, N, hi - lo, -1).permute(0, 2, 1, 3))
        return torch.cat(ks, 1), torch.cat(vs, 1)


# ---- helpers shared by training and evaluation --------------------------------------------------------------------------

def rope_cache(model, k_pre: torch.Tensor, v: torch.Tensor, rd: dict) -> DynamicCache:
    """k_pre, v: [B, L, N, H*d] -> a DynamicCache with the receiver's RoPE at positions 0..N-1."""
    from transformers.models.llama.modeling_llama import apply_rotary_pos_emb
    B, L, N, _ = k_pre.shape
    kk = k_pre.reshape(B, L, N, rd["H"], rd["d"]).permute(0, 1, 3, 2, 4)  # [B, L, H, N, d]
    vv = v.reshape(B, L, N, rd["H"], rd["d"]).permute(0, 1, 3, 2, 4)
    pos = torch.arange(N, device=k_pre.device)[None].expand(B, -1)
    cache = DynamicCache()
    for l in range(L):
        cos, sin = model.model.rotary_emb(kk[:, l], pos)
        _, kr = apply_rotary_pos_emb(kk[:, l], kk[:, l], cos, sin)
        cache.update(kr.contiguous(), vv[:, l].contiguous(), l)
    return cache


def cont_nll(model, cache, ctx_mask, c_ids, c_mask, per_item=False):
    """Receiver reads only the cache; NLL of continuation tokens 1.. given tokens 0..; ctx_mask marks real cache entries."""
    B, Q = c_ids.shape
    ctx_len = ctx_mask.sum(1)
    att = torch.cat([ctx_mask, c_mask], 1).long()
    pos = ctx_len[:, None] + torch.arange(Q, device=c_ids.device)[None]
    out = model(input_ids=c_ids, attention_mask=att, position_ids=pos, past_key_values=cache, use_cache=True)
    lp = F.cross_entropy(out.logits[:, :-1].float().reshape(-1, out.logits.shape[-1]), c_ids[:, 1:].reshape(-1), reduction="none").view(B, Q - 1)
    m = (c_mask[:, 1:] & c_mask[:, :-1]).float()
    per = (lp * m).sum(1) / m.sum(1).clamp_min(1)
    return per if per_item else (lp * m).sum() / m.sum().clamp_min(1)


class Setup:
    """Frozen sender and receiver, hooks, the mapper and its calibration."""

    def __init__(self, sender_path: str, receiver_path: str, B=7, D=1024):
        self.sm, self.st = load(sender_path)
        self.rm, self.rt = load(receiver_path)
        self.sd, self.rd = kv_dims(self.sm), kv_dims(self.rm)
        self.gs, self.gr = GrabB(self.sm), GrabB(self.rm)
        self.mapper = Mapper(self.sd, self.rd, B=B, D=D).to(DEV)
        self.pad_s = self.st.pad_token_id if self.st.pad_token_id is not None else 0
        self.pad_r = self.rt.pad_token_id if self.rt.pad_token_id is not None else 0

    @torch.no_grad()
    def sender_cache(self, bt):
        self.sm(input_ids=bt["s_ids"], use_cache=False)
        return self.gs.stack()

    @torch.no_grad()
    def receiver_cache(self, bt):
        self.rm(input_ids=bt["r_ids"], use_cache=False)
        return self.gr.stack()

    @torch.no_grad()
    def calibrate(self, pool: Pool, n_batches=8, bs=16):
        acc = torch.zeros(self.rd["L"], 2, self.rd["H"], device=DEV)
        for i in range(n_batches):
            bt = pool.batch(random.sample(range(len(pool)), bs), self.pad_s, self.pad_r)
            rk, rv = self.receiver_cache(bt)
            m = bt["r_mask"][:, None, :, None].float()
            for j, t in enumerate((rk, rv)):
                x = t.float().reshape(t.shape[0], t.shape[1], t.shape[2], self.rd["H"], self.rd["d"])
                rms = (x ** 2).mean(-1).sqrt()  # [B, L, N, H]
                acc[:, j] += ((rms * m[..., :1]).sum((0, 2)) / m.sum() / 1.0)
        self.mapper.sigma.copy_(acc / n_batches)

    def predict(self, bt, sk, sv):
        r_emb = self.rm.get_input_embeddings()(bt["r_ids"]).float()
        with torch.autocast(DEV, dtype=torch.bfloat16, enabled=DEV == "cuda"):
            return self.mapper(r_emb, sk.float(), sv.float(), bt["pos"], bt["cross"])


# ---- training ---------------------------------------------------------------------------------------------------------

def mse_loss(su: Setup, bt, pk, pv, rk, rv):
    """Normalised MSE to the receiver's own cache, each layer and head divided by its calibrated RMS (valid tokens only)."""
    m = bt["r_mask"][:, None, :, None].float()  # [B, 1, N, 1]
    tot = 0.0
    for j, (p, t) in enumerate(((pk, rk), (pv, rv))):
        sh = lambda x: x.float().reshape(x.shape[0], x.shape[1], x.shape[2], su.rd["H"], su.rd["d"])
        s = su.mapper.sigma[:, j][None, :, None, :, None].clamp_min(1e-6)
        d = (((sh(p) - sh(t)) / s) ** 2).mean(-1)  # [B, L, N, H]
        tot = tot + (d * m).sum() / (m.sum() * d.shape[1] * d.shape[3])
    return tot / 2


def train(sender_path, receiver_path, out_path, steps1=3000, steps2=8000, bs=48, lr1=3e-4, lr2=5e-5, n_items=100_000,
          B=7, D=1024, eval_every=1000, time_budget_s: float | None = None, log=print, seed=0):
    t0 = time.time()
    torch.manual_seed(seed); random.seed(seed)
    su = Setup(sender_path, receiver_path, B, D)
    n_wiki = int(n_items * 0.85)
    texts = wiki_texts("wikitext-103-raw-v1", "train")
    items = build_items(texts, n_wiki, seed) + templated_items(n_items - n_wiki, seed)
    random.Random(seed).shuffle(items)
    pool = Pool(items, su.st, su.rt)
    ev_items = build_items(wiki_texts("wikitext-103-raw-v1", "validation"), 200, 1)
    evp = Pool(ev_items, su.st, su.rt)
    log(f"pool {len(pool)} train items, {len(evp)} eval items, mapper {sum(p.numel() for p in su.mapper.parameters()) / 1e6:.0f}M params [{time.time() - t0:.0f}s]")
    su.calibrate(pool)
    opt = torch.optim.AdamW(su.mapper.parameters(), lr=lr1, weight_decay=1e-3, betas=(0.9, 0.95))
    hist = []

    def lr_at(step, total, base, warm=100):
        if step < warm:
            return base * (step + 1) / warm
        return base * (0.1 + 0.9 * 0.5 * (1 + math.cos(math.pi * (step - warm) / max(1, total - warm))))

    order = list(range(len(pool)))
    random.shuffle(order)
    ptr = 0

    def next_idx():
        nonlocal ptr
        if ptr + bs > len(order):
            random.shuffle(order); ptr = 0
        ptr += bs
        return order[ptr - bs: ptr]

    total = steps1 + steps2
    for step in range(total):
        if time_budget_s and time.time() - t0 > time_budget_s:
            log(f"time budget reached at step {step}"); break
        stage = 1 if step < steps1 else 2
        for g in opt.param_groups:
            g["lr"] = lr_at(step if stage == 1 else step - steps1, steps1 if stage == 1 else steps2, lr1 if stage == 1 else lr2)
        bt = pool.batch(next_idx(), su.pad_s, su.pad_r)
        sk, sv = su.sender_cache(bt)
        if stage == 1:
            rk, rv = su.receiver_cache(bt)
            pk, pv = su.predict(bt, sk, sv)
            loss = mse_loss(su, bt, pk, pv, rk, rv)
        else:
            pk, pv = su.predict(bt, sk, sv)
            cache = rope_cache(su.rm, pk.to(torch.bfloat16), pv.to(torch.bfloat16), su.rd)
            loss = cont_nll(su.rm, cache, bt["r_mask"], bt["c_ids"], bt["c_mask"])
        opt.zero_grad(set_to_none=True)
        loss.backward()
        torch.nn.utils.clip_grad_norm_(su.mapper.parameters(), 1.0)
        opt.step()
        if step % 50 == 0:
            log(f"step {step} stage {stage} loss {loss.item():.4f} lr {opt.param_groups[0]['lr']:.2e} [{time.time() - t0:.0f}s]")
        if (step + 1) % eval_every == 0 or step + 1 == total:
            e = quick_eval(su, evp, 64)
            e.update(step=step + 1, stage=stage, seconds=round(time.time() - t0))
            hist.append(e)
            log("eval", json.dumps(e))
            torch.save({"mapper": su.mapper.state_dict(), "sd": su.sd, "rd": su.rd, "B": B, "D": D, "hist": hist,
                        "sender": sender_path, "receiver": receiver_path}, out_path)
    torch.save({"mapper": su.mapper.state_dict(), "sd": su.sd, "rd": su.rd, "B": B, "D": D, "hist": hist,
                "sender": sender_path, "receiver": receiver_path}, out_path)
    return hist


@torch.no_grad()
def quick_eval(su: Setup, pool: Pool, n: int, bs=16):
    """Receiver NLL on held-out continuations: real cache vs mapped cache vs no cache (bf16, batched)."""
    sums = {"none": 0.0, "own": 0.0, "mapped": 0.0}
    cnt = 0
    su.mapper.eval()
    for i in range(0, min(n, len(pool)), bs):
        idx = list(range(i, min(i + bs, n, len(pool))))
        bt = pool.batch(idx, su.pad_s, su.pad_r)
        sk, sv = su.sender_cache(bt)
        rk, rv = su.receiver_cache(bt)
        pk, pv = su.predict(bt, sk, sv)
        k = len(idx)
        sums["none"] += float(cont_nll(su.rm, None, torch.zeros(k, 0, dtype=torch.bool, device=DEV), bt["c_ids"], bt["c_mask"], True).sum())
        sums["own"] += float(cont_nll(su.rm, rope_cache(su.rm, rk, rv, su.rd), bt["r_mask"], bt["c_ids"], bt["c_mask"], True).sum())
        sums["mapped"] += float(cont_nll(su.rm, rope_cache(su.rm, pk.to(rk.dtype), pv.to(rv.dtype), su.rd), bt["r_mask"], bt["c_ids"], bt["c_mask"], True).sum())
        cnt += k
    su.mapper.train()
    r = {k: round(v / cnt, 4) for k, v in sums.items()}
    r["retention"] = round((r["none"] - r["mapped"]) / (r["none"] - r["own"]), 4)
    return r


def load_mapper(ckpt_path: str, sender_path=None, receiver_path=None, dtype=torch.float32) -> "Setup":
    ck = torch.load(ckpt_path, map_location="cpu")
    su = Setup(sender_path or ck["sender"], receiver_path or ck["receiver"], ck["B"], ck["D"])
    su.mapper.load_state_dict(ck["mapper"])
    su.mapper.eval()
    return su
