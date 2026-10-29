"""A fast path for E6: every arm and every context goes through the model in one batch.

The slow path (`e6b.run_cell`) runs one forward pass per (arm, context), then one per query and per generated token.
Here a batch row is an (arm, context) pair and the steering vector is per row: the clean arm's row gets zero, every
other arm's row gets its own direction. One forward reads all contexts under all arms; the same batch then reads each
query and generates greedily, so the model is called once per step for the whole experiment instead of once per
arm and context.

  * rows are left-padded; positions come from the attention mask, so a padded row is the same computation as the
    unpadded one;
  * the context cache is shared by the queries and rewound with `DynamicCache.crop`, so nothing is copied;
  * only the last position's logits are computed when the model supports `num_logits_to_keep`;
  * `device` and `dtype` are options (cpu/float32 reproduces the slow path; mps/bfloat16 is faster and is not
    bit-reproducible, so results on it are reported as a separate setting).

Equivalence with the slow path on the same device and dtype is tested in tests/test_fast.py.
"""
from __future__ import annotations

import inspect
import time
from pathlib import Path

import torch
from transformers import AutoModelForCausalLM, AutoTokenizer, DynamicCache

from . import engine, latent
from .e6b import QUERIES, Result, lexicon_score, summarize
from .e6 import TRAITS


def load_fast(weights: Path, device: str = "cpu", dtype: str = "float32"):
    torch.set_grad_enabled(False)
    tok = AutoTokenizer.from_pretrained(weights, local_files_only=True)
    tok.padding_side = "left"
    if tok.pad_token is None:
        tok.pad_token = tok.eos_token
    model = AutoModelForCausalLM.from_pretrained(weights, local_files_only=True, torch_dtype=getattr(torch, dtype))
    return model.to(device).eval(), tok


def _dev(model):
    return next(model.parameters()).device


def _cache(past):
    return past if isinstance(past, DynamicCache) else DynamicCache.from_legacy_cache(past)


def _keeps_last(model) -> bool:
    return "num_logits_to_keep" in inspect.signature(model.forward).parameters


class _RowSteer:
    """Adds a per-row vector to the output of one block, on the context pass only."""

    def __init__(self, model, layer: int, rows: torch.Tensor | None):
        self.block, self.rows, self.h = engine.blocks(model)[layer], rows, None

    def __enter__(self):
        if self.rows is not None:
            def hook(_m, _i, out):
                if isinstance(out, tuple):
                    return (out[0] + self.rows.to(out[0].dtype),) + tuple(out[1:])
                return out + self.rows.to(out.dtype)
            self.h = self.block.register_forward_hook(hook)
        return self

    def __exit__(self, *_):
        if self.h:
            self.h.remove()


def _positions(mask: torch.Tensor) -> torch.Tensor:
    return (mask.long().cumsum(-1) - 1).clamp(min=0)


def _capture(store: dict):
    """A forward hook that records the block output. It must return None: a returned value replaces the output."""
    def hook(_m, _i, out):
        store["o"] = (out[0] if isinstance(out, tuple) else out).detach()
    return hook


def calibrate_fast(model, tok, ctxs: list[str], layer: int) -> float:
    """Mean L2 norm of the block output over non-first real tokens of all contexts (pooled), as `engine.calibrate`."""
    dev = _dev(model)
    enc = tok(ctxs, return_tensors="pt", padding=True).to(dev)
    seen = {}
    h = engine.blocks(model)[layer].register_forward_hook(_capture(seen))
    try:
        model(input_ids=enc.input_ids, attention_mask=enc.attention_mask, position_ids=_positions(enc.attention_mask), use_cache=False)
    finally:
        h.remove()
    norms = seen["o"].float().norm(dim=-1)  # (B, T)
    mask = enc.attention_mask.bool().clone()
    first = mask.float().argmax(-1)  # index of each row's first real token
    mask[torch.arange(len(ctxs)), first] = False
    keep = mask if mask.any() else enc.attention_mask.bool()
    return float(norms[keep].mean())


def diffmean_fast(model, tok, positive: list[str], negative: list[str], layer: int) -> torch.Tensor:
    dev = _dev(model)
    out = []
    for group in (positive, negative):
        enc = tok(group, return_tensors="pt", padding=True).to(dev)
        seen = {}
        h = engine.blocks(model)[layer].register_forward_hook(_capture(seen))
        try:
            model(input_ids=enc.input_ids, attention_mask=enc.attention_mask, position_ids=_positions(enc.attention_mask), use_cache=False)
        finally:
            h.remove()
        out.append(seen["o"][:, -1].float().mean(0))  # left padding: the last position is each row's last real token
    d = out[0] - out[1]
    return (d / d.norm()).cpu()


def run_rows(model, tok, rows_ctx: list[str], rows_dir: torch.Tensor, scale: float, layer: int, queries: list[str],
             pos_ids, neg_ids, trait: str, gen_tokens: int) -> list[list[Result]]:
    """One batch: row i reads rows_ctx[i] with `scale * rows_dir[i]` added at `layer`. Returns, per row, a Result per query."""
    dev = _dev(model)
    dtype = next(model.parameters()).dtype
    enc = tok(rows_ctx, return_tensors="pt", padding=True).to(dev)
    ids, mask = enc.input_ids, enc.attention_mask
    n, t0 = ids.shape
    steer = (scale * rows_dir).to(dev, dtype).unsqueeze(1)  # (N, 1, H)
    with _RowSteer(model, layer, steer):
        out = model(input_ids=ids, attention_mask=mask, position_ids=_positions(mask), use_cache=True)
    base = _cache(out.past_key_values)
    lens = mask.sum(-1, keepdim=True)  # real tokens per row
    keep = {"num_logits_to_keep": 1} if _keeps_last(model) else {}
    results: list[list[Result]] = [[] for _ in range(n)]
    for q in queries:
        qids = torch.tensor(tok(q).input_ids, device=dev).unsqueeze(0).repeat(n, 1)
        ql = qids.shape[1]
        full = torch.cat([mask, torch.ones(n, ql, dtype=mask.dtype, device=dev)], 1)
        step = model(input_ids=qids, attention_mask=full, position_ids=lens + torch.arange(ql, device=dev), past_key_values=base, use_cache=True, **keep)
        past = _cache(step.past_key_values)  # models with older cache code return a new object instead of updating in place
        logits = step.logits[:, -1].float()
        lp = torch.log_softmax(logits, -1)
        tl = (torch.logsumexp(lp[:, pos_ids], 1) - torch.logsumexp(lp[:, neg_ids], 1)).cpu()
        cur, gen, m, pos = logits, [], full, lens + ql
        for _ in range(gen_tokens):
            nxt = cur.argmax(-1)
            gen.append(nxt)
            m = torch.cat([m, torch.ones(n, 1, dtype=m.dtype, device=dev)], 1)
            s2 = model(input_ids=nxt.unsqueeze(1), attention_mask=m, position_ids=pos, past_key_values=past, use_cache=True, **keep)
            past = _cache(s2.past_key_values)
            cur, pos = s2.logits[:, -1].float(), pos + 1
        gen = torch.stack(gen, 1).cpu() if gen else torch.zeros(n, 0, dtype=torch.long)
        for i in range(n):
            toks = gen[i].tolist()
            if tok.eos_token_id in toks:
                toks = toks[: toks.index(tok.eos_token_id)]
            results[i].append(Result(float(tl[i]), lexicon_score(tok.decode(toks, skip_special_tokens=True), trait), logits[i].cpu()))
        base.crop(t0)  # rewind to the end of the context for the next query (a no-op if the model did not touch `base`)
    return results


def run_cell_fast(model, tok, ctxs, trait: str, trait_dir, rand_dirs, sem_dirs: dict, layer: int, coef: float, gen_tokens: int, max_rows: int = 256) -> dict:
    t = TRAITS[trait]
    pos_ids, neg_ids = latent.word_ids(tok, t["pos_words"]), latent.word_ids(tok, t["neg_words"])
    scale = coef * calibrate_fast(model, tok, list(ctxs), layer)
    queries = QUERIES[trait]
    H = trait_dir.numel()
    arms = {"clean": torch.zeros(H), "steered": trait_dir, **{f"rand_{k}": d for k, d in enumerate(rand_dirs)}, **{f"sem_{n}": d for n, d in sem_dirs.items()}}
    rows = [(a, ci) for a in arms for ci in range(len(ctxs))]
    flat: list[list[Result]] = []
    for s in range(0, len(rows), max_rows):
        chunk = rows[s : s + max_rows]
        flat += run_rows(model, tok, [ctxs[ci] for _, ci in chunk], torch.stack([arms[a] for a, _ in chunk]), scale, layer, queries, pos_ids, neg_ids, trait, gen_tokens)
    res: dict[str, list[Result]] = {a: [] for a in arms}
    for (a, _), r in zip(rows, flat):
        res[a] += r  # context-major, query-minor: the slow path's order
    return summarize(res)


def timed(fn, *a, **k):
    t0 = time.perf_counter()
    out = fn(*a, **k)
    return out, time.perf_counter() - t0


def main(argv=None) -> None:
    """python -m playbox_steer.fast --weights DIR --out report.json [--device mps --dtype bfloat16] [--bench]

    The same grid as `e6b`, through the batched path. `--bench` also times one cell through the slow path
    (only sensible for small models) and prints the speedup."""
    import argparse
    import json

    from . import e6b
    from .e6 import contexts

    ap = argparse.ArgumentParser()
    ap.add_argument("--weights", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--device", default="cpu")
    ap.add_argument("--dtype", default="float32")
    ap.add_argument("--layers", default="4,8")
    ap.add_argument("--coefs", default="0.25,0.5")
    ap.add_argument("--nulls", type=int, default=16)
    ap.add_argument("--contexts", type=int, default=32)
    ap.add_argument("--gen-tokens", type=int, default=20)
    ap.add_argument("--traits", default="sentiment,fear")
    ap.add_argument("--max-rows", type=int, default=256)
    ap.add_argument("--bench", action="store_true")
    a = ap.parse_args(argv)
    model, tok = load_fast(Path(a.weights), a.device, a.dtype)
    dim = model.config.hidden_size
    out = {"model": a.weights, "device": a.device, "dtype": a.dtype, "n_layers": len(engine.blocks(model)), "layers": {}}
    ctxs = contexts(a.contexts)
    for layer in [int(x) for x in a.layers.split(",")]:
        rand = [torch.tensor(engine.random_direction(dim, 100 + k)) for k in range(a.nulls)]
        sem = {n: diffmean_fast(model, tok, p, q, layer) for n, (p, q) in e6b.SEMANTIC.items()}
        out["layers"][str(layer)] = {}
        for trait in a.traits.split(","):
            t = TRAITS[trait]
            d = diffmean_fast(model, tok, t["positive"], t["negative"], layer)
            out["layers"][str(layer)][trait] = {}
            for c in [float(x) for x in a.coefs.split(",")]:
                r, secs = timed(run_cell_fast, model, tok, ctxs, trait, d, rand, sem, layer, c, a.gen_tokens, a.max_rows)
                r["seconds"] = secs
                if a.bench:
                    _, slow = timed(e6b.run_cell, model, tok, ctxs, trait, d, rand, sem, layer, c, a.gen_tokens)
                    r["seconds_slow_path"], r["speedup"] = slow, slow / secs
                out["layers"][str(layer)][trait][str(c)] = r
                g, n = r["gen"], r["trait_logit"]
                print(f"L{layer:<2} {trait:9} {c:>4}: next-token {n['effect']['mean']:+.2f} (sem as far {n['semantic_as_far']:.2f}) | "
                      f"generation {g['effect']['mean']:+.4f} [{g['effect']['low']:+.4f},{g['effect']['high']:+.4f}] (sem as far {g['semantic_as_far']:.2f}, rand as far {g['random_as_far']:.2f}) | "
                      f"KL {r['kl_steered']:.3f} (rand {r['kl_random_mean']:.3f}, sem {r['kl_semantic_mean']:.3f}) | {secs:.1f}s" + (f" vs slow {r['seconds_slow_path']:.1f}s = {r['speedup']:.1f}x" if a.bench else ""), flush=True)
            Path(a.out).write_text(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
