"""Cross-family latent channel between two agents' models, text-free.

Sender A (family X) holds a private concept as a few sentences. It reads them, takes the mean mid-layer state minus the
mean state of neutral sentences (a "thought" vector), and sends ONLY that vector. A ridge map fitted on sentence-level
mean states of the two models (different families, tokenizers and widths, so pairs are whole sentences, not tokens)
turns it into receiver B's space. B adds it at one layer while continuing neutral prompts. Nothing but the vector
crosses. A decoder then tries to name the concept from B's text.

Arms, same strength (in B's own mean token norm at its layer):
    mapped   A's thought vector through the fitted map
    direct   B's own diffmean for the concept (upper bound; needs B to have seen the concept text)
    null     random unit vectors in A's space through the same map (what an arbitrary vector looks like after the map)
    clean    nothing
Concepts used for testing are never in the map's fit set.
"""
from __future__ import annotations

import json
import os
import random
import sys
from pathlib import Path

import torch
from transformers import AutoModelForCausalLM, AutoTokenizer

from .engine import blocks
from .telepathy_data import FIT_TOPICS, NEUTRAL, PROMPTS, TEST_CONCEPTS

DEVICE = os.environ.get("PLAYBOX_DEVICE", "cpu")  # "cuda" for big models; states and vectors always come back on the CPU


def load(path: str):
    torch.set_grad_enabled(False)
    tok = AutoTokenizer.from_pretrained(path, local_files_only=True)
    model = AutoModelForCausalLM.from_pretrained(path, local_files_only=True, torch_dtype=torch.float32).eval().to(DEVICE)
    return model, tok


def layer_of(model, frac: float) -> int:
    return int(len(blocks(model)) * frac)


def sentence_states(model, tok, texts: list[str], layer: int) -> torch.Tensor:
    """Mean over non-first tokens of the block output, one row per text."""
    rows, cur = [], {}
    h = blocks(model)[layer].register_forward_hook(
        lambda _m, _i, out: cur.__setitem__("x", (out[0] if isinstance(out, tuple) else out).detach()))
    try:
        for t in texts:
            ids = tok(t, return_tensors="pt").input_ids[:, :64].to(model.device)
            model(input_ids=ids, use_cache=False)
            x = cur["x"][0].cpu()
            rows.append(x[1:].mean(0) if x.shape[0] > 1 else x[0])
    finally:
        h.remove()
    return torch.stack(rows)


def mean_norm(model, tok, texts: list[str], layer: int) -> float:
    cur, norms = {}, []
    h = blocks(model)[layer].register_forward_hook(
        lambda _m, _i, out: cur.__setitem__("x", (out[0] if isinstance(out, tuple) else out).detach()))
    try:
        for t in texts:
            model(input_ids=tok(t, return_tensors="pt").input_ids[:, :64].to(model.device), use_cache=False)
            norms.append(cur["x"][0, 1:].norm(dim=-1).mean().cpu())
    finally:
        h.remove()
    return float(torch.stack(norms).mean())


class Ridge:
    def __init__(self, xa: torch.Tensor, xb: torch.Tensor, lam: float):
        self.ma, self.mb = xa.mean(0), xb.mean(0)
        a, b = xa - self.ma, xb - self.mb
        n = a.shape[0]
        # dual form: W = A^T (A A^T + lam*I)^-1 B, cheap when n < d_in
        g = a @ a.T
        scale = g.diagonal().mean()
        self.w = a.T @ torch.linalg.solve(g + lam * scale * torch.eye(n), b)

    def direction(self, d: torch.Tensor) -> torch.Tensor:
        return d @ self.w  # a difference of means: the centering cancels


def fit_map(ma, ta, la, mb, tb, lb, lam_grid=(0.03, 0.1, 0.3)) -> tuple[Ridge, dict]:
    if os.environ.get("TELEPATHY_FIT") == "big":
        from .telepathy_data_big import FIT_TOPICS_BIG
        fit_topics = FIT_TOPICS_BIG
    else:
        fit_topics = FIT_TOPICS
    texts = [s for topic in fit_topics.values() for s in topic]
    random.Random(0).shuffle(texts)
    held, train = texts[:150], texts[150:]
    xa, xb = sentence_states(ma, ta, train, la), sentence_states(mb, tb, train, lb)
    ha, hb = sentence_states(ma, ta, held, la), sentence_states(mb, tb, held, lb)
    best, report = None, {}
    for lam in lam_grid:
        r = Ridge(xa, xb, lam)
        pred = (ha - r.ma) @ r.w + r.mb
        cos = torch.nn.functional.cosine_similarity(pred - r.mb, hb - r.mb, dim=-1).mean().item()
        report[str(lam)] = round(cos, 4)
        if best is None or cos > best[0]:
            best = (cos, lam)
    xa_all = torch.cat([xa, ha]); xb_all = torch.cat([xb, hb])
    return Ridge(xa_all, xb_all, best[1]), {"heldout_cosine_by_lambda": report, "lambda": best[1], "n_fit": len(texts)}


def thought(model, tok, layer: int, concept_texts: list[str], neutral_states: torch.Tensor) -> torch.Tensor:
    return sentence_states(model, tok, concept_texts, layer).mean(0) - neutral_states.mean(0)


def generate(model, tok, layer: int, vec: torch.Tensor | None, prompt: str, n: int) -> str:
    ids = tok(prompt, return_tensors="pt").input_ids.to(model.device)
    h = None
    if vec is not None:
        def hook(_m, _i, out):
            if isinstance(out, tuple):
                return (out[0] + vec.to(out[0].device, out[0].dtype),) + tuple(out[1:])
            return out + vec.to(out.device, out.dtype)
        h = blocks(model)[layer].register_forward_hook(hook)
    try:
        out = model.generate(ids, max_new_tokens=n, do_sample=False, pad_token_id=tok.eos_token_id,
                             repetition_penalty=1.1)
    finally:
        if h:
            h.remove()
    return tok.decode(out[0, ids.shape[1]:], skip_special_tokens=True)


def lexicon_id(text: str, lexicons: dict[str, set[str]]) -> tuple[str | None, dict]:
    words = [w.strip(".,;:!?\"'()").lower() for w in text.split()]
    hits = {c: sum(w in lx for w in words) for c, lx in lexicons.items()}
    top = max(hits.values())
    winners = [c for c, v in hits.items() if v == top]
    return (winners[0] if top > 0 and len(winners) == 1 else None), hits


def run(path_a: str, path_b: str, frac: float, coefs: list[float], n_null: int, gen: int, out: Path | None) -> dict:
    ma, ta = load(path_a); mb, tb = load(path_b)
    la, lb = layer_of(ma, frac), layer_of(mb, frac)
    ridge, fitinfo = fit_map(ma, ta, la, mb, tb, lb)
    neu_a, neu_b = sentence_states(ma, ta, NEUTRAL, la), sentence_states(mb, tb, NEUTRAL, lb)
    norm_b = mean_norm(mb, tb, NEUTRAL + PROMPTS, lb)
    concepts = list(TEST_CONCEPTS)
    lex = {c: set(v["lexicon"]) for c, v in TEST_CONCEPTS.items()}
    gen_cache: dict[tuple, str] = {}
    g = torch.Generator().manual_seed(0)

    def decode(vec, key):
        outs = []
        for p in PROMPTS:
            k = (key, p)
            if k not in gen_cache:
                gen_cache[k] = generate(mb, tb, lb, vec, p, gen)
            outs.append(gen_cache[k])
        return outs

    results = {"fit": fitinfo, "layers": {"a": la, "b": lb}, "coefs": {}}
    clean = decode(None, "clean")
    clean_ids = [lexicon_id(t, lex)[0] for t in clean]
    results["clean_text"] = clean[:2]
    for coef in coefs:
        arms = {"mapped": [], "direct": [], "null": []}
        samples = {}
        for c in concepts:
            sa = thought(ma, ta, la, TEST_CONCEPTS[c]["sentences"], neu_a)
            mapped = ridge.direction(sa)
            direct = thought(mb, tb, lb, TEST_CONCEPTS[c]["sentences"], neu_b)
            for arm, v in (("mapped", mapped), ("direct", direct)):
                vec = v / v.norm() * coef * norm_b
                texts = decode(vec, (arm, c, coef))
                arms[arm] += [(c, lexicon_id(t, lex)) for t in texts]
                samples[f"{arm}:{c}"] = texts[0]
        for i in range(n_null):
            r = torch.randn(ma.config.hidden_size, generator=g)
            r = r / r.norm() * sentence_states(ma, ta, NEUTRAL, la).norm(dim=-1).mean()
            v = ridge.direction(r)
            vec = v / v.norm() * coef * norm_b
            texts = decode(vec, ("null", i, coef))
            for t in texts:
                arms["null"].append((concepts[i % len(concepts)], lexicon_id(t, lex)))
        summary = {}
        for arm, rows in arms.items():
            n = len(rows)
            correct = sum(1 for c, (pred, _) in rows if pred == c)
            named = sum(1 for _, (pred, _) in rows if pred is not None)
            tot_hits = sum(h[c] for c, (_, h) in rows)
            summary[arm] = {"n": n, "id_acc": correct / n, "named": named / n, "target_hits_per_text": tot_hits / n}
        results["coefs"][str(coef)] = {"summary": summary, "samples": samples}
    results["chance"] = 1 / len(concepts)
    results["clean_id_acc_vs_nothing"] = sum(x is not None for x in clean_ids) / len(clean_ids)
    if out:
        out.write_text(json.dumps(results, indent=1))
    return results


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--a", required=True); ap.add_argument("--b", required=True)
    ap.add_argument("--frac", type=float, default=0.45)
    ap.add_argument("--coefs", default="0.5,1.0,2.0")
    ap.add_argument("--nulls", type=int, default=12)
    ap.add_argument("--gen", type=int, default=24)
    ap.add_argument("--out")
    a = ap.parse_args()
    r = run(a.a, a.b, a.frac, [float(x) for x in a.coefs.split(",")], a.nulls, a.gen, Path(a.out) if a.out else None)
    print(json.dumps({"fit": r["fit"], "chance": r["chance"], **{k: v["summary"] for k, v in r["coefs"].items()}}, indent=1))
