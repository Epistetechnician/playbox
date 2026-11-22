"""Hugging Face side of the metered benchmark: the receiver wrapper (cached prefix, steering hook) and the latent sender, plus `run`, which
loads a sender and a receiver, fits the mapper (timed), builds the shared world and runs every arm under one Meter."""
from __future__ import annotations

import json
import time
from pathlib import Path

import torch

from . import telepathy as T
from .concepts_natural import ALL_CONCEPTS
from .engine import blocks
from .kv_hybrid import BM25
from .metered_bench import ARMS, Meter, build_world, run_arm, summarize
from .telepathy_data import NEUTRAL, PROMPTS


class HFIO:
    def __init__(self, model, tok, layer: int, fixed_decode: bool = True):
        """fixed_decode: every call emits exactly n tokens (EOS suppressed), so GPU-seconds differ between arms only by how the context
        arrived, not by how early a short answer ends the generation. The first run left it off and the arms that answer briefly
        looked cheaper than the ones that ramble; results/metered/*_variable_decode.json keep that run."""
        self.model, self.tok, self.layer, self.fixed = model, tok, layer, fixed_decode

    def ids(self, text: str, special: bool = True):
        return self.tok(text, return_tensors="pt", add_special_tokens=special).input_ids.to(self.model.device)

    def prefill(self, ids):
        pkv = self.model(input_ids=ids, use_cache=True).past_key_values
        if not hasattr(pkv, "crop"):  # older model classes hand back the legacy tuple
            from transformers import DynamicCache
            if hasattr(DynamicCache, "from_legacy_cache"):
                pkv = DynamicCache.from_legacy_cache(pkv)
            else:
                cache = DynamicCache()
                for idx, (k, v) in enumerate(pkv):
                    cache.update(k, v, idx)
                pkv = cache
        return pkv

    def generate(self, ids, n: int, cache=None, vec=None, prefix_ids=None) -> str:
        full = torch.cat([prefix_ids, ids], dim=1) if prefix_ids is not None else ids
        h = None
        if vec is not None:
            v = vec

            def hook(_m, _i, out):
                if isinstance(out, tuple):
                    return (out[0] + v.to(out[0].device, out[0].dtype),) + tuple(out[1:])
                return out + v.to(out.device, out.dtype)
            h = blocks(self.model)[self.layer].register_forward_hook(hook)
        try:
            kw = {"past_key_values": cache} if cache is not None else {}
            out = self.model.generate(full, max_new_tokens=n, do_sample=False, pad_token_id=self.tok.eos_token_id,
                                      repetition_penalty=1.1 if vec is not None else 1.0,
                                      **({"min_new_tokens": n} if self.fixed else {}), **kw)
        finally:
            if h:
                h.remove()
            if cache is not None:
                excess = cache.get_seq_length() - prefix_ids.shape[1]
                if excess > 0:
                    cache.crop(-excess)  # back to the shared prefix: the next turn reuses it untouched
        return self.tok.decode(out[0, full.shape[1]:], skip_special_tokens=True)


class Latent:
    def __init__(self, ma, ta, la, ridge, neu_a, norm_b, coef: float, concepts: dict):
        self.ma, self.ta, self.la, self.ridge, self.neu_a, self.norm_b, self.coef, self.concepts = ma, ta, la, ridge, neu_a, norm_b, coef, concepts
        self.free_prompts = PROMPTS

    def vector(self, concept: str):
        d = self.ridge.direction(T.thought(self.ma, self.ta, self.la, self.concepts[concept]["sentences"], self.neu_a))
        return (d / d.norm() * self.coef * self.norm_b).float()


def run(sender_path: str, receiver_path: str, n_sessions: int = 40, frac: float = 0.45, coef: float = 0.75, rate_usd_h: float = 1.10,
        amortise_sessions: int = 1000, arms=ARMS, out: Path | None = None, rag_k: int = 5, hybrid_k: int = 10, fact_turns: int = 7, seed: int = 0) -> dict:
    cuda = torch.cuda.is_available() and T.DEVICE == "cuda"
    sync = torch.cuda.synchronize if cuda else (lambda: None)
    ma, ta = T.load(sender_path)
    mb, tb = T.load(receiver_path)
    la, lb = T.layer_of(ma, frac), T.layer_of(mb, frac)
    sync()
    t0 = time.perf_counter()
    ridge, info = T.fit_map(ma, ta, la, mb, tb, lb)
    sync()
    fit_s = time.perf_counter() - t0
    neu_a = T.sentence_states(ma, ta, NEUTRAL, la)
    norm_b = T.mean_norm(mb, tb, NEUTRAL + PROMPTS, lb)
    latent = Latent(ma, ta, la, ridge, neu_a, norm_b, coef, ALL_CONCEPTS)
    io = HFIO(mb, tb, lb)
    chunks, sessions = build_world(n_sessions, lambda t: len(tb(t, add_special_tokens=False).input_ids), ALL_CONCEPTS, seed=seed, fact_turns=fact_turns)
    bm = BM25([c["text"] for c in chunks])
    meter = Meter(sync)
    io.generate(io.ids("warm up the kernels"), 4)  # one throwaway call so the first arm does not pay for CUDA start-up
    res = {}
    for arm in arms:
        res[arm] = run_arm(arm, io, chunks, bm, sessions, ALL_CONCEPTS, meter, latent=latent, rag_k=rag_k, hybrid_k=hybrid_k, lexicon_id=T.lexicon_id)
        print(arm, {k: (v["correct"], v["n"]) for k, v in summarize({arm: res[arm]}, meter, n_sessions, rate_usd_h)[arm].items() if k in ("topic", "fact")}, flush=True)
    summ = summarize(res, meter, n_sessions, rate_usd_h, fit_s=fit_s, amortise_sessions=amortise_sessions)
    result = {"sender": sender_path, "receiver": receiver_path, "n_sessions": n_sessions, "fact_turns": len(sessions[0]["turns"]) - 1,
              "corpus_chunks": len(chunks), "corpus_tokens": sum(len(tb(c["text"], add_special_tokens=False).input_ids) for c in chunks),
              "rate_usd_h": rate_usd_h, "amortise_sessions": amortise_sessions, "fit_s": round(fit_s, 2), "map": info, "layer": {"sender": la, "receiver": lb},
              "coef": coef, "corpus_seed": seed, "device": torch.cuda.get_device_name(0) if cuda else "cpu", "arms": summ,
              "samples": {a: [r["out"] for r in res[a]["rows"][:4]] for a in res}}
    if out:
        Path(out).write_text(json.dumps(result, indent=1))
    return result
