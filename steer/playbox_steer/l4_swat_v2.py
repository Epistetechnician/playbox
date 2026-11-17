"""Mapper recipe v2 for the blind cross-family cache mapper (written from the ideas in the OneTriangle cache-transfer post, not from any code):

  1. Ridge start. A closed-form ridge map from the aligned sender cache to the receiver cache is fitted first (per layer group, uncentred
     features rescaled per column, intercept kept apart). The learned mapper only adds a residual on top of it, and its output layers start at
     zero, so training begins AT the ridge map instead of at random. Aim: remove the training-seed spread seen in docs/l4-scope.md section 12.
  2. KL loss. Stage 2 trains the receiver's next-token DISTRIBUTION on the continuation to match the receiver reading the real context cache
     (teacher), instead of the receiver's NLL on the true next token.
  3. Key weight. Stage 1 weights the key reconstruction error `key_weight` times the value error (keys carry the attention routing).
  4. Edge recompute. The first context token's K/V is the receiver's own (it depends on that token alone, so this leaks nothing), and the last
     `edge_k` context tokens are not mapped at all: they are fed to the receiver as input ahead of the continuation, attending to the mapped
     cache, exactly as a real receiver would compute them. Their own predictions are not scored.

All four are switches (`V2Config`), so each can be ablated. The model, data, calibration and evaluation code of l4_swat.py are reused."""
from __future__ import annotations

import json
import random
import time
from dataclasses import asdict, dataclass

import torch
import torch.nn as nn
import torch.nn.functional as F

from . import l4_swat as S

DEV = S.DEV


@dataclass
class V2Config:
    ridge: bool = True
    loss: str = "kl"        # "kl" or "nll" for stage 2
    key_weight: float = 2.0  # stage-1 weight of the key error relative to the value error
    edge_k: int = 4          # last context tokens read natively by the receiver (0 = off)
    ridge_lam: float = 1e-2  # relative ridge penalty (times the mean diagonal of the Gram matrix)
    ridge_batches: int = 160


# ---- alignment ----------------------------------------------------------------------------------------------------------

def align_index(pos_s: torch.Tensor, cross: torch.Tensor, n_recv: int) -> torch.Tensor:
    """For each receiver token j, the sender token whose fractional position is nearest to j among the sender tokens it may see.
    pos_s [B, S], cross [B, N, S] -> [B, N] indices."""
    j = torch.arange(n_recv, device=pos_s.device, dtype=pos_s.dtype)[None, :, None]
    d = (pos_s[:, None, :] - j).abs().masked_fill(~cross, 1e9)
    return d.argmin(-1)


def layer_flat(k: torch.Tensor, v: torch.Tensor, lo: int, hi: int) -> torch.Tensor:
    """k, v [B, L, T, kd] -> [B, T, (hi-lo)*2*kd], per layer [K, V]."""
    return torch.stack([k[:, lo:hi], v[:, lo:hi]], 2).permute(0, 3, 1, 2, 4).reshape(k.shape[0], k.shape[2], -1)


# ---- mapper -------------------------------------------------------------------------------------------------------------

class RidgeMapper(S.Mapper):
    """S.Mapper plus a frozen ridge prior per layer group. prediction = ridge(aligned sender cache) + transformer residual."""

    def __init__(self, sd, rd, B=7, D=1024, H=16, Fd=4096, blind=False, use_ridge=True):
        super().__init__(sd, rd, B=B, D=D, H=H, Fd=Fd, blind=blind)
        self.use_ridge = use_ridge
        self.fitted = False
        for b, blk in enumerate(self.blocks):  # zero-initialised output layers: at step 0 the residual is exactly zero
            nn.init.zeros_(blk.out.weight)
            nn.init.zeros_(blk.out.bias)
        for b in range(B):
            n_in = (self.sg[b][1] - self.sg[b][0]) * 2 * sd["kd"]
            n_out = (self.rg[b][1] - self.rg[b][0]) * 2 * rd["kd"]
            self.register_buffer(f"W{b}", torch.zeros(n_in, n_out))
            self.register_buffer(f"xs{b}", torch.ones(n_in))
            self.register_buffer(f"c{b}", torch.zeros(n_in))
            self.register_buffer(f"b{b}", torch.zeros(n_out))

    def ridge_part(self, b: int, sk, sv, idx):
        lo, hi = self.sg[b]
        x = layer_flat(sk, sv, lo, hi)                                   # [B, S, in]
        x = torch.gather(x, 1, idx[:, :, None].expand(-1, -1, x.shape[-1]))  # aligned: [B, N, in]
        x = (x - getattr(self, f"c{b}")) / getattr(self, f"xs{b}")
        with torch.autocast(x.device.type, enabled=False):  # the prior stays in float32 under bf16 autocast
            return x.float() @ getattr(self, f"W{b}") + getattr(self, f"b{b}")  # [B, N, out]

    def forward(self, r_emb, sk, sv, pos_s, cross):
        B_, N, _ = r_emb.shape
        if self.blind:
            r_emb = self.blind_emb.to(r_emb.dtype).expand_as(r_emb)
        h = self.w_in(r_emb)
        pos_r = torch.arange(N, device=h.device)[None].expand(B_, -1).float()
        idx = align_index(pos_s, cross, N) if self.use_ridge else None
        ks, vs = [], []
        for b, blk in enumerate(self.blocks):
            lo, hi = self.sg[b]
            mem = torch.cat([sk[:, lo:hi], sv[:, lo:hi]], 1).permute(0, 2, 1, 3).reshape(B_, sk.shape[2], -1)
            h, out = blk(h, mem, pos_r, pos_s, cross)
            rlo, rhi = self.rg[b]
            o = out.view(B_, N, rhi - rlo, 2, self.rd["H"], self.rd["d"]) * self.sigma[rlo:rhi][None, None, :, :, :, None].to(out.dtype)
            if self.use_ridge:
                o = o + self.ridge_part(b, sk, sv, idx).to(o.dtype).view(B_, N, rhi - rlo, 2, self.rd["H"], self.rd["d"])
            ks.append(o[:, :, :, 0].reshape(B_, N, rhi - rlo, -1).permute(0, 2, 1, 3))
            vs.append(o[:, :, :, 1].reshape(B_, N, rhi - rlo, -1).permute(0, 2, 1, 3))
        return torch.cat(ks, 1), torch.cat(vs, 1)


@torch.no_grad()
def fit_ridge(su: S.Setup, pool: S.Pool, cfg: V2Config, bs: int = 16, log=print) -> dict:
    """Closed-form ridge per layer group from `cfg.ridge_batches` batches. Features: aligned sender cache, centred and scaled per column.
    Target: the receiver's real cache at the same token. Returns fit diagnostics (held-out R^2 on the last batches)."""
    m = su.mapper
    B = m.B
    sums = [None] * B
    held = []
    n_hold = max(2, cfg.ridge_batches // 10)
    for it in range(cfg.ridge_batches):
        bt = pool.batch(random.sample(range(len(pool)), min(bs, len(pool))), su.pad_s, su.pad_r)
        sk, sv = su.sender_cache(bt)
        rk, rv = su.receiver_cache(bt)
        idx = align_index(bt["pos_b"] if su.blind else bt["pos"], bt["cross_b"] if su.blind else bt["cross"], bt["r_ids"].shape[1])
        valid = bt["r_mask"]
        for b in range(B):
            lo, hi = m.sg[b]
            rlo, rhi = m.rg[b]
            x = torch.gather(layer_flat(sk, sv, lo, hi).float(), 1, idx[:, :, None].expand(-1, -1, (hi - lo) * 2 * m.sd["kd"]))[valid]
            y = layer_flat(rk, rv, rlo, rhi).float()[valid]
            if it >= cfg.ridge_batches - n_hold:
                held.append((b, x, y))
                continue
            if sums[b] is None:
                sums[b] = {"n": 0, "sx": torch.zeros(x.shape[1], device=DEV), "sxx": torch.zeros(x.shape[1], device=DEV),
                           "sy": torch.zeros(y.shape[1], device=DEV), "xtx": torch.zeros(x.shape[1], x.shape[1], device=DEV),
                           "xty": torch.zeros(x.shape[1], y.shape[1], device=DEV)}
            s = sums[b]
            s["n"] += x.shape[0]
            s["sx"] += x.sum(0)
            s["sxx"] += (x * x).sum(0)
            s["sy"] += y.sum(0)
            s["xtx"] += x.T @ x
            s["xty"] += x.T @ y
    r2 = []
    for b in range(B):
        s = sums[b]
        n = s["n"]
        mx, my = s["sx"] / n, s["sy"] / n
        xs = ((s["sxx"] / n - mx * mx).clamp_min(1e-12)).sqrt()
        # centred, scaled Gram matrices from the raw sums:  Z = (X - mx) / xs
        xtx = (s["xtx"] - n * torch.outer(mx, mx)) / torch.outer(xs, xs)
        xty = (s["xty"] - n * torch.outer(mx, my)) / xs[:, None]
        lam = cfg.ridge_lam * xtx.diagonal().mean()
        eye = torch.eye(xtx.shape[0], device=DEV)
        W = torch.linalg.solve(xtx + lam * eye, xty)
        getattr(m, f"W{b}").copy_(W)
        getattr(m, f"xs{b}").copy_(xs)
        getattr(m, f"c{b}").copy_(mx)
        getattr(m, f"b{b}").copy_(my)
        hx = torch.cat([x for bb, x, _ in held if bb == b])
        hy = torch.cat([y for bb, _, y in held if bb == b])
        pred = ((hx - mx) / xs) @ W + my
        r2.append(float(1 - ((pred - hy) ** 2).sum() / ((hy - hy.mean(0)) ** 2).sum()))
        del xtx, xty, s
    m.fitted = True
    log(f"ridge fitted: held-out R^2 per group {[round(v, 3) for v in r2]}")
    return {"ridge_r2_by_group": [round(v, 4) for v in r2], "n_tokens": n}


# ---- the receiver reading a mapped cache ---------------------------------------------------------------------------------

def with_edge(bt: dict, k: int):
    """Per item: the last k context tokens move from the cache to the front of the receiver's input. Returns
    (ctx_mask [B, N], ids [B, k+Q], in_mask [B, k+Q], score_mask [B, k+Q-1] over targets 1.., true only for continuation targets)."""
    r_ids, r_mask, c_ids, c_mask = bt["r_ids"], bt["r_mask"], bt["c_ids"], bt["c_mask"]
    B, N = r_ids.shape
    Q = c_ids.shape[1]
    n = r_mask.sum(1)
    ke = torch.clamp(torch.full_like(n, k), max=(n - 1))   # never the first token: it stays in the cache
    ids = torch.zeros(B, k + Q, dtype=torch.long, device=r_ids.device)
    in_mask = torch.zeros(B, k + Q, dtype=torch.bool, device=r_ids.device)
    ctx_mask = r_mask.clone()
    for b in range(B):
        e, nb = int(ke[b]), int(n[b])
        nc = int(c_mask[b].sum())
        if e:
            ids[b, :e] = r_ids[b, nb - e:nb]
            ctx_mask[b, nb - e:nb] = False
        ids[b, e:e + nc] = c_ids[b, :nc]
        in_mask[b, :e + nc] = True
    tgt = torch.arange(k + Q, device=r_ids.device)[None]
    score = (tgt >= ke[:, None]) & in_mask                  # targets that are continuation tokens
    return ctx_mask, ids, in_mask, score[:, 1:] & in_mask[:, 1:] & in_mask[:, :-1]


def logits_with(model, cache_kv, ctx_mask, ids, in_mask, rd, real_first=None):
    """Receiver forward over `ids` attending to a cache built from cache_kv=(K,V) [B, L, N, kd] masked by ctx_mask (None = no cache)."""
    if cache_kv is None:
        cache, cm = None, torch.zeros(ids.shape[0], 0, dtype=torch.bool, device=ids.device)
    else:
        k, v = cache_kv
        if real_first is not None:  # the first token's K/V are the receiver's own
            k, v = k.clone(), v.clone()
            k[:, :, 0], v[:, :, 0] = real_first[0][:, :, 0], real_first[1][:, :, 0]
        cache, cm = S.rope_cache(model, k, v, rd), ctx_mask
    att = torch.cat([cm, in_mask], 1).long()
    pos = cm.sum(1)[:, None] + torch.arange(ids.shape[1], device=ids.device)[None]
    return model(input_ids=ids, attention_mask=att, position_ids=pos, past_key_values=cache, use_cache=True).logits


def masked_nll(logits, ids, score):
    lp = F.cross_entropy(logits[:, :-1].float().reshape(-1, logits.shape[-1]), ids[:, 1:].reshape(-1), reduction="none").view(score.shape)
    m = score.float()
    return (lp * m).sum() / m.sum().clamp_min(1)


def masked_kl(student, teacher, score):
    """KL(teacher || student) per scored position, mean."""
    ls = F.log_softmax(student[:, :-1].float(), -1)
    lt = F.log_softmax(teacher[:, :-1].float(), -1)
    kl = (lt.exp() * (lt - ls)).sum(-1)
    m = score.float()
    return (kl * m).sum() / m.sum().clamp_min(1)


def mse_loss_kw(su: S.Setup, bt, pk, pv, rk, rv, key_weight: float):
    """S.mse_loss with the key error weighted (keys: key_weight, values: 1, normalised so the weights sum to 2)."""
    m = bt["r_mask"][:, None, :, None].float()
    tot = 0.0
    for j, (p, t, w) in enumerate(((pk, rk, key_weight), (pv, rv, 1.0))):
        sh = lambda x: x.float().reshape(x.shape[0], x.shape[1], x.shape[2], su.rd["H"], su.rd["d"])
        s = su.mapper.sigma[:, j][None, :, None, :, None].clamp_min(1e-6)
        d = (((sh(p) - sh(t)) / s) ** 2).mean(-1)
        tot = tot + w * (d * m).sum() / (m.sum() * d.shape[1] * d.shape[3])
    return tot / (key_weight + 1.0)


def stage2_loss(su: S.Setup, bt, pk, pv, rk, rv, cfg: V2Config):
    """Receiver reading the mapped cache (edge tokens native, first token real) against the chosen target."""
    ctx_mask, ids, in_mask, score = with_edge(bt, cfg.edge_k) if cfg.edge_k else (bt["r_mask"], bt["c_ids"], bt["c_mask"], None)
    if score is None:
        score = (in_mask[:, 1:] & in_mask[:, :-1])
    student = logits_with(su.rm, (pk.to(rk.dtype), pv.to(rv.dtype)), ctx_mask, ids, in_mask, su.rd, real_first=(rk, rv))
    if cfg.loss == "nll":
        return masked_nll(student, ids, score)
    with torch.no_grad():  # teacher: the same receiver reading the REAL cache of the whole context, same inputs
        teacher = logits_with(su.rm, (rk, rv), ctx_mask, ids, in_mask, su.rd)
    return masked_kl(student, teacher, score)


# ---- setup, training, loading ---------------------------------------------------------------------------------------------

def make_setup(sender_path, receiver_path, B, D, cfg: V2Config, blind=True, dtype=torch.bfloat16) -> S.Setup:
    su = S.Setup(sender_path, receiver_path, B, D, dtype=dtype, blind=blind)
    su.mapper = RidgeMapper(su.sd, su.rd, B=B, D=D, blind=blind, use_ridge=cfg.ridge).to(DEV)
    return su


@torch.enable_grad()  # other entry points switch autograd off process-wide (engine.load, hub_run); training must not inherit that
def train(sender_path, receiver_path, out_path, cfg: V2Config, steps1=1500, steps2=4000, bs=32, lr1=3e-4, lr2=5e-5, n_items=50_000, B=4, D=768,
          eval_every=500, time_budget_s=None, log=print, seed=0, blind=True, templ_frac=0.15, items=None, eval_items=None, dtype=torch.bfloat16):
    """Same data and schedule as S.train (so a comparison with the v1 recipe changes only the recipe)."""
    t0 = time.time()
    torch.manual_seed(seed); random.seed(seed)
    su = make_setup(sender_path, receiver_path, B, D, cfg, blind=blind, dtype=dtype)
    if items is None:
        n_wiki = int(n_items * (1 - templ_frac))
        items = S.build_items(S.wiki_texts("wikitext-103-raw-v1", "train"), n_wiki, seed) + S.templated_items(n_items - n_wiki, seed)
        random.Random(seed).shuffle(items)
        eval_items = S.build_items(S.wiki_texts("wikitext-103-raw-v1", "validation"), 200, 1)
    pool, evp = S.Pool(items, su.st, su.rt), S.Pool(eval_items, su.st, su.rt)
    log(f"pool {len(pool)} train items, {len(evp)} eval items, cfg {json.dumps(asdict(cfg))} [{time.time() - t0:.0f}s]")
    su.calibrate(pool)
    fit_info = fit_ridge(su, pool, cfg, log=log) if cfg.ridge else {}
    log(f"ridge/prior ready [{time.time() - t0:.0f}s]")
    opt = torch.optim.AdamW([p for p in su.mapper.parameters() if p.requires_grad], lr=lr1, weight_decay=1e-3, betas=(0.9, 0.95))
    hist, bad = [], 0
    order = list(range(len(pool)))
    random.shuffle(order)
    ptr = 0

    def next_idx():
        nonlocal ptr
        if ptr + bs > len(order):
            random.shuffle(order); ptr = 0
        ptr += bs
        return order[ptr - bs: ptr]

    import math

    def lr_at(step, total, base, warm=100):
        if step < warm:
            return base * (step + 1) / warm
        return base * (0.1 + 0.9 * 0.5 * (1 + math.cos(math.pi * (step - warm) / max(1, total - warm))))

    def save():
        torch.save({"mapper": su.mapper.state_dict(), "sd": su.sd, "rd": su.rd, "B": B, "D": D, "blind": blind, "hist": hist, "v2": asdict(cfg),
                    "fit": fit_info, "sender": sender_path, "receiver": receiver_path}, out_path)

    total = steps1 + steps2
    for step in range(total):
        if bad > 200:
            log(f"stopping at step {step}: {bad} non-finite batches, the run has diverged")
            break
        if time_budget_s and time.time() - t0 > time_budget_s:
            log(f"time budget reached at step {step}"); break
        stage = 1 if step < steps1 else 2
        for g in opt.param_groups:
            g["lr"] = lr_at(step if stage == 1 else step - steps1, steps1 if stage == 1 else steps2, lr1 if stage == 1 else lr2)
        bt = pool.batch(next_idx(), su.pad_s, su.pad_r)
        sk, sv = su.sender_cache(bt)
        rk, rv = su.receiver_cache(bt)
        pk, pv = su.predict(bt, sk, sv)
        loss = mse_loss_kw(su, bt, pk, pv, rk, rv, cfg.key_weight) if stage == 1 else stage2_loss(su, bt, pk, pv, rk, rv, cfg)
        opt.zero_grad(set_to_none=True)
        if not torch.isfinite(loss):
            bad += 1
            log(f"step {step} non-finite loss, batch skipped ({bad} so far)")
            continue
        loss.backward()
        gn = torch.nn.utils.clip_grad_norm_(su.mapper.parameters(), 1.0)
        if not torch.isfinite(gn):
            bad += 1
            opt.zero_grad(set_to_none=True)
            log(f"step {step} non-finite gradient, batch skipped ({bad} so far)")
            continue
        opt.step()
        if step % 50 == 0:
            log(f"step {step} stage {stage} loss {loss.item():.4f} lr {opt.param_groups[0]['lr']:.2e} [{time.time() - t0:.0f}s]")
        if (step + 1) % eval_every == 0 or step + 1 == total:
            e = S.quick_eval(su, evp, 64)
            e.update(step=step + 1, stage=stage, seconds=round(time.time() - t0))
            hist.append(e)
            log("eval", json.dumps(e))
            save()
    save()
    return hist


def load_mapper(ckpt_path: str, sender_path=None, receiver_path=None, dtype=torch.float32) -> S.Setup:
    ck = torch.load(ckpt_path, map_location="cpu")
    cfg = V2Config(**ck["v2"])
    su = make_setup(sender_path or ck["sender"], receiver_path or ck["receiver"], ck["B"], ck["D"], cfg, blind=ck.get("blind", True), dtype=dtype)
    su.mapper.load_state_dict(ck["mapper"])
    su.mapper.eval()
    su.v2 = cfg
    return su
