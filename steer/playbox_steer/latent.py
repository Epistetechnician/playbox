"""E6 (docs/steering.md): can a steering effect reach another agent through the KV-cache while the text it reads
is identical?

Two agents share one base model, so a cache can be handed over directly and no mapper is needed. Agent A reads
a context under a steering spec (`tokens: prompt`, so only the context's positions are edited). Agent B then
continues from A's cache with its own query. B never sees steered *text*: in every arm the context and query
strings are the same. Any difference in B's behaviour between the steered arm and the clean arm therefore travelled
through the cache, where text-level checks cannot see it.

Arms, per context:
  clean         A reads the context unsteered. This equals B reading the text itself (checked in a test).
  steered       A reads the context with the trait direction added; B continues from that cache.
  null_<k>      the same with a random unit direction of matched strength.
  text_steered  for comparison: A generates a continuation while steered, and B reads that *text* (unsteered).
Metrics, on B's side only:
  trait_logit   log-mass of positive minus negative words at the next token after the query (no judge needed).
  proj          mean projection of B's query-position states on a probe direction, in units of that layer's norm.
  kl            KL(arm || clean) of the next-token distribution at the end of the query, in nats.
"""
from __future__ import annotations

import copy
import math
from dataclasses import dataclass
from types import SimpleNamespace

import numpy as np
import torch

from . import engine
from .spec import Spec

POSITIVE_WORDS = [" great", " good", " wonderful", " excellent", " amazing", " fantastic", " lovely", " love"]
NEGATIVE_WORDS = [" terrible", " bad", " awful", " horrible", " poor", " disappointing", " dreadful", " hate"]
ZERO = "0" * 64


def word_ids(tok, words: list[str]) -> list[int]:
    out = []
    for w in words:
        ids = tok(w).input_ids
        if len(ids) == 1:  # only words that are a single token, so one next-token logit scores each
            out.append(ids[0])
    if not out:
        raise engine.SteerError("none of the scoring words is a single token for this tokenizer")
    return out


def _spec(layer: int, coef: float, tokens: str) -> Spec:
    return Spec(model="e6", weights=ZERO, site="resid_post", layers=(layer,), direction=ZERO, recipe="diffmean",
                mode="add", coef_milli=int(round(coef * 1000)), tokens=tokens, prompts=ZERO, seed=0)


def read_context(model, tok, text: str, steerer: engine.Steerer | None):
    """A reads `text` once and returns its KV-cache."""
    ids = tok(text, return_tensors="pt").input_ids
    if steerer:
        steerer.phase = "prefill"
    past = model(input_ids=ids, use_cache=True).past_key_values
    return past


@dataclass
class Handoff:
    trait_logit: float
    proj: float
    logits: torch.Tensor  # next-token logits at the end of the query


def continue_from(model, tok, past, query: str, pos_ids, neg_ids, probe_dir: torch.Tensor | None, probe_layer: int, probe_norm: float) -> Handoff:
    """B reads `query` on top of a cache (a private copy, so arms never contaminate each other)."""
    ids = tok(query, return_tensors="pt").input_ids
    out = model(input_ids=ids, past_key_values=copy.deepcopy(past), use_cache=True, output_hidden_states=True)
    logits = out.logits[0, -1]
    lp = torch.log_softmax(logits, -1)
    tl = float(torch.logsumexp(lp[pos_ids], 0) - torch.logsumexp(lp[neg_ids], 0))
    proj = 0.0
    if probe_dir is not None:
        h = out.hidden_states[probe_layer + 1][0]  # output of block `probe_layer`
        proj = float((h @ probe_dir).mean() / probe_norm)
    return Handoff(tl, proj, logits.clone())


def kl(p_logits: torch.Tensor, q_logits: torch.Tensor) -> float:
    lp, lq = torch.log_softmax(p_logits, -1), torch.log_softmax(q_logits, -1)
    return float((lp.exp() * (lp - lq)).sum())


def bootstrap_ci(x: np.ndarray, n: int = 2000, seed: int = 0) -> tuple[float, float, float]:
    rng = np.random.default_rng(seed)
    means = np.array([x[rng.integers(0, len(x), len(x))].mean() for _ in range(n)])
    return float(x.mean()), float(np.percentile(means, 2.5)), float(np.percentile(means, 97.5))


def run_e6(model, tok, contexts: list[str], query: str, trait_dir: torch.Tensor, null_dirs: list[torch.Tensor],
           steer_layer: int, probe_dir: torch.Tensor, probe_layer: int, coef: float, gen_tokens: int = 16,
           positive_words: list[str] | None = None, negative_words: list[str] | None = None) -> dict:
    """Everything for one (layer, coefficient). Returns per-context values and summary statistics."""
    pos_ids, neg_ids = word_ids(tok, positive_words or POSITIVE_WORDS), word_ids(tok, negative_words or NEGATIVE_WORDS)
    calib = SimpleNamespace(prompts=tuple(contexts))
    base_spec = _spec(steer_layer, coef, "prompt")
    norm_ref = engine.calibrate(model, tok, base_spec, calib)
    probe_norm = engine.calibrate(model, tok, _spec(probe_layer, 0, "prompt"), calib)[probe_layer]

    def latent(direction: torch.Tensor | None, ctx: str) -> Handoff:
        steerer = engine.Steerer(model, base_spec, direction, norm_ref) if direction is not None else None
        if steerer:
            with steerer:
                past = read_context(model, tok, ctx, steerer)
        else:
            past = read_context(model, tok, ctx, None)
        return continue_from(model, tok, past, query, pos_ids, neg_ids, probe_dir, probe_layer, probe_norm)

    def text_from(direction: torch.Tensor, ctx: str) -> Handoff:
        spec = _spec(steer_layer, coef, "all")
        with engine.Steerer(model, spec, direction, norm_ref) as s:
            written, _ = engine.generate(model, tok, s, ctx, gen_tokens, 0.0, 0)
        past = read_context(model, tok, ctx + written, None)  # B reads A's steered text, unsteered
        return continue_from(model, tok, past, query, pos_ids, neg_ids, probe_dir, probe_layer, probe_norm)

    arms: dict[str, list[Handoff]] = {"clean": [latent(None, c) for c in contexts]}
    arms["steered"] = [latent(trait_dir, c) for c in contexts]
    for k, nd in enumerate(null_dirs):
        arms[f"null_{k}"] = [latent(nd, c) for c in contexts]
    arms["text_steered"] = [text_from(trait_dir, c) for c in contexts]

    clean = arms["clean"]

    def stats(name: str) -> dict:
        hs = arms[name]
        d_tl = np.array([h.trait_logit - c.trait_logit for h, c in zip(hs, clean)])
        d_pr = np.array([h.proj - c.proj for h, c in zip(hs, clean)])
        k = np.array([kl(h.logits, c.logits) for h, c in zip(hs, clean)])
        m, lo, hi = bootstrap_ci(d_tl)
        pm, plo, phi = bootstrap_ci(d_pr)
        return {"trait_logit_effect": {"mean": m, "low": lo, "high": hi}, "proj_effect": {"mean": pm, "low": plo, "high": phi},
                "kl_mean": float(k.mean()), "n": len(hs)}

    report = {"arms": {n: stats(n) for n in arms if n != "clean"}}
    null_names = [n for n in arms if n.startswith("null_")]
    steered = report["arms"]["steered"]["trait_logit_effect"]["mean"]
    if null_names:
        # Share of random directions whose effect is at least as large (in magnitude) as the trait direction's.
        report["null_exceed_fraction"] = float(np.mean([abs(report["arms"][n]["trait_logit_effect"]["mean"]) >= abs(steered) for n in null_names]))
        # Share whose signed effect is at least as large: how often a random direction pushes at least as far the same way.
        report["null_signed_exceed_fraction"] = float(np.mean([report["arms"][n]["trait_logit_effect"]["mean"] >= steered for n in null_names]))
        # The trait-specific part: per context, steered effect minus the mean effect of all random directions.
        eff = lambda n: np.array([h.trait_logit - c.trait_logit for h, c in zip(arms[n], clean)])
        specific = eff("steered") - np.mean([eff(n) for n in null_names], axis=0)
        m, lo, hi = bootstrap_ci(specific)
        report["specific_effect"] = {"mean": m, "low": lo, "high": hi}
        report["null_effect_mean"] = float(np.mean([report["arms"][n]["trait_logit_effect"]["mean"] for n in null_names]))
    report["config"] = {"steer_layer": steer_layer, "probe_layer": probe_layer, "coef": coef, "n_contexts": len(contexts),
                        "nulls": len(null_dirs), "query": query}
    # Sanity: the clean latent arm must equal B reading the same text itself, bit for bit.
    ids = tok(contexts[0] + query, return_tensors="pt").input_ids
    own = model(input_ids=ids, use_cache=False).logits[0, -1]
    report["sanity_clean_equals_own_read_max_delta"] = float((own - clean[0].logits).abs().max())
    return report
