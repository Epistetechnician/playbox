"""E3 from docs/steering.md: does a direction found in a small model steer a bigger one through a learned linear map?

    small model:  d_s = diffmean(positive, negative) at (site, layer_s)
    fit:          ridge map W (d_s_dim -> d_b_dim) on the two models' activations for the SAME tokens of the SAME
                  text (the models must share a tokenizer)
    transfer:     d_b' = unit(d_s W)  applied to the big model at (site, layer_b)

Controls, each applied to the big model at the same strength:
    direct        the big model's own diffmean direction (what transfer is trying to match)
    mapped_null   unit random vectors in the SMALL space, pushed through the same map (a map can make any vector
                  look structured; this is what a random direction looks like after the map)
    random_null   unit random vectors in the BIG space
    clean         no steering

Strength is in the big model's own mean activation norm at its layer (`Steerer` semantics), so arms are comparable.
Text is generated greedily; the scorer is separate (judge/experiments/e3_score.py), so nothing here knows the answer.
"""
from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

import torch

from . import engine
from .files import PromptSet, hash_bytes
from .spec import Spec

MAX_FIT_TOKENS = 128


def token_states(model, tok, texts: list[str], site: str, layer: int, max_len: int = MAX_FIT_TOKENS) -> torch.Tensor:
    """Activations at (site, layer) for every token of every text except the first, which is an attention sink with a
    very different norm. Returns [N, width]."""
    bs = engine.blocks(model)
    cur: dict = {}
    h = engine.site_module(bs[layer], site).register_forward_hook(lambda _m, _i, out: cur.__setitem__("x", engine._tensor(out).detach()))
    rows = []
    try:
        for t in texts:
            ids = tok(t, return_tensors="pt").input_ids[:, :max_len]
            if ids.shape[1] < 2:
                continue
            model(input_ids=ids, use_cache=False)
            rows.append(cur["x"][0, 1:].clone())
    finally:
        h.remove()
    return torch.cat(rows)


@dataclass
class LinearMap:
    w: torch.Tensor      # [d_in, d_out]
    mean_in: torch.Tensor
    mean_out: torch.Tensor

    def apply(self, x: torch.Tensor) -> torch.Tensor:
        return (x - self.mean_in) @ self.w + self.mean_out

    def direction(self, v: torch.Tensor) -> torch.Tensor:
        """A difference of activations maps through the linear part only; returns a unit vector."""
        out = v.to(self.w.dtype) @ self.w
        n = out.norm()
        if not torch.isfinite(n) or n == 0:
            raise ValueError("the direction maps to zero")
        return out / n


def r2_score(y: torch.Tensor, yhat: torch.Tensor) -> float:
    """Variance-weighted R^2 over output dimensions: 1 - SSres / SStot (can be negative)."""
    ss_res = ((y - yhat) ** 2).sum()
    ss_tot = ((y - y.mean(0, keepdim=True)) ** 2).sum()
    return float(1.0 - ss_res / ss_tot)


def fit_map(x: torch.Tensor, y: torch.Tensor, ridge: float = 1e-3, holdout: float = 0.2, seed: int = 0) -> tuple[LinearMap, dict]:
    """Centered ridge regression y ~ x W, with the penalty relative to the mean variance of x. Reports R^2 on rows held
    out of the fit (a random split; rows from the same text can fall on both sides, so this is optimistic)."""
    if x.shape[0] != y.shape[0]:
        raise ValueError("x and y need the same rows")
    n = x.shape[0]
    g = torch.Generator().manual_seed(seed)
    perm = torch.randperm(n, generator=g)
    cut = int(n * (1 - holdout))
    tr, te = perm[:cut], perm[cut:]

    def solve(rows):
        xt, yt = x[rows].double(), y[rows].double()
        mx, my = xt.mean(0), yt.mean(0)
        xc, yc = xt - mx, yt - my
        lam = ridge * xc.pow(2).sum() / xc.shape[1]
        a = xc.T @ xc + lam * torch.eye(xc.shape[1], dtype=torch.float64)
        return LinearMap(torch.linalg.solve(a, xc.T @ yc), mx, my)

    m = solve(tr)
    stats = {
        "rows": n, "train_rows": int(cut), "heldout_rows": int(n - cut), "ridge": ridge,
        "r2_train": r2_score(y[tr].double(), m.apply(x[tr].double())),
        "r2_heldout": r2_score(y[te].double(), m.apply(x[te].double())),
    }
    full = solve(torch.arange(n))
    return LinearMap(full.w.float(), full.mean_in.float(), full.mean_out.float()), stats


def cosine(a: torch.Tensor, b: torch.Tensor) -> float:
    return float((a * b).sum() / (a.norm() * b.norm()))


def _spec(model_label: str, site: str, layer: int, coef: float) -> Spec:
    z = "0" * 64
    return Spec(model=model_label, weights=z, site=site, layers=(layer,), direction=z, recipe="diffmean", mode="add",
                coef_milli=round(coef * 1000), tokens="all", prompts=z, seed=0)


def generate_arm(model, tok, direction: torch.Tensor | None, spec: Spec, norm_ref: dict, prompts: list[str], n_tokens: int) -> dict:
    """Greedy text for every prompt under one direction (None: clean), plus mean per-token KL(steered || clean)."""
    texts, kls = [], []
    for p in prompts:
        if direction is None:
            t, _ = engine.generate(model, tok, None, p, n_tokens, 0.0, 0)
            texts.append(t)
            continue
        trace: list = []
        with engine.Steerer(model, spec, direction, norm_ref) as st:
            t, ids = engine.generate(model, tok, st, p, n_tokens, 0.0, 0, trace)
        texts.append(t)
        kls.append(engine.mean_kl(trace, engine.teacher_forced_logits(model, tok, p, ids, None)))
    return {"texts": texts, "kl": (sum(kls) / len(kls)) if kls else 0.0}


def run_e3(
    small_dir: Path, big_dir: Path, *, site: str, small_layer: int, big_layer: int,
    positive: list[str], negative: list[str], fit_texts: list[str], eval_prompts: list[str],
    coef: float, nulls: int, tokens: int, shard: tuple[int, int] = (0, 1), ridge: float = 1e-3,
    log=lambda _m: None,
) -> dict:
    small, stok = engine.load(small_dir)
    big, btok = engine.load(big_dir)
    if stok.get_vocab() != btok.get_vocab():
        raise ValueError("the two models need the same tokenizer: the map is fit on activations for identical tokens")
    d_small = engine.last_token_states(small, stok, positive, site, small_layer)
    d_small = d_small[: len(positive)].mean(0) - engine.last_token_states(small, stok, negative, site, small_layer).mean(0)
    d_big = engine.last_token_states(big, btok, positive, site, big_layer).mean(0) - engine.last_token_states(big, btok, negative, site, big_layer).mean(0)
    log("extracted directions")
    x = token_states(small, stok, fit_texts, site, small_layer)
    y = token_states(big, btok, fit_texts, site, big_layer)
    if x.shape[0] != y.shape[0]:
        raise ValueError("the models tokenized the fit text differently")
    lm, fit = fit_map(x, y, ridge=ridge)
    log(f"fit map: {fit}")

    direct = d_big / d_big.norm()
    mapped = lm.direction(d_small)
    info = {
        "fit": fit,
        "cosine_mapped_vs_direct": cosine(mapped, direct),
        "small_dim": int(d_small.numel()), "big_dim": int(d_big.numel()),
    }
    # How well does the map carry arbitrary vectors? Cosine of mapped-null with the direct direction is the chance level.
    g = torch.Generator()
    arms: dict[str, torch.Tensor] = {"direct": direct, "mapped": mapped}
    cos_null = []
    for k in range(nulls):
        g.manual_seed(1000 + k)
        v = torch.randn(d_small.numel(), generator=g)
        arms[f"mapped_null{k}"] = lm.direction(v / v.norm())
        cos_null.append(cosine(arms[f"mapped_null{k}"], direct))
        g.manual_seed(2000 + k)
        r = torch.randn(d_big.numel(), generator=g)
        arms[f"random_null{k}"] = r / r.norm()
    info["cosine_mapped_null_vs_direct"] = cos_null

    spec = _spec("big", site, big_layer, coef)
    ps = PromptSet(tuple(eval_prompts), tokens, 0.0, hash_bytes(json.dumps(eval_prompts).encode()))
    norm_ref = engine.calibrate(big, btok, spec, ps)
    names = [n for n in arms if (list(arms).index(n) % shard[1]) == shard[0]]
    out = {"info": info, "coef": coef, "tokens": tokens, "site": site, "layers": [small_layer, big_layer], "norm_ref": norm_ref[big_layer], "arms": {}}
    out["arms"]["clean"] = generate_arm(big, btok, None, spec, norm_ref, eval_prompts, tokens)
    for n in names:
        out["arms"][n] = generate_arm(big, btok, arms[n], spec, norm_ref, eval_prompts, tokens)
        log(f"arm {n}: kl={out['arms'][n]['kl']:.3f}")
    return out
