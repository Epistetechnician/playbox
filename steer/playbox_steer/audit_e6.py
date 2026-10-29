"""Evaluate the cache audit (cache_audit.py) on a real model.

    python -m playbox_steer.audit_e6 --weights DIR --out report.json [--layer 4] [--contexts 32]

Honest handoffs must pass; steered ones must fail. Reports the false-positive count at a zero and at a small
tolerance, the smallest steering strength caught, how large honest numerical noise is when the auditor uses a
different thread count (so the tolerance can be chosen), and what the audit costs next to the handoff itself.
"""
from __future__ import annotations

import argparse
import json
import time
from pathlib import Path
from types import SimpleNamespace

import torch
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

from . import cache_audit as ca
from . import engine, latent
from .e6 import contexts


def main(argv=None) -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--weights", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--layer", type=int, default=4)
    ap.add_argument("--contexts", type=int, default=32)
    ap.add_argument("--coefs", default="0.001,0.01,0.05,0.25,1,4")
    a = ap.parse_args(argv)
    w = Path(a.weights)
    model, tok = engine.load(w)
    key = Ed25519PrivateKey.generate()
    pub = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    weights = "00" * 32
    ctxs = contexts(a.contexts)
    dim = model.config.hidden_size
    d = torch.tensor(engine.random_direction(dim, 7))
    clean = lambda t: latent.read_context(model, tok, t, None)

    def steered(text, coef):
        spec = latent._spec(a.layer, coef, "prompt")
        norm = engine.calibrate(model, tok, spec, SimpleNamespace(prompts=tuple(ctxs)))
        with engine.Steerer(model, spec, d, norm):
            return latent.read_context(model, tok, text, None)

    report = {"model": str(w), "layer": a.layer, "contexts": len(ctxs)}
    fp0 = fp3 = 0
    noise = []
    t_audit, t_continue = [], []
    for c in ctxs:
        past = clean(c)
        rec = ca.commit(key, weights, c, past)
        fp0 += not ca.verify(rec, c, past, weights, {pub}, recompute=clean, tol=0.0).ok
        fp3 += not ca.verify(rec, c, past, weights, {pub}, recompute=clean, tol=1e-3).ok
        t0 = time.perf_counter()
        ca.verify(rec, c, past, weights, {pub}, recompute=clean, tol=1e-3)
        t_audit.append(time.perf_counter() - t0)
        ids = tok(" Honestly, I think it was", return_tensors="pt").input_ids
        t0 = time.perf_counter()
        model(input_ids=ids, past_key_values=past, use_cache=True)
        t_continue.append(time.perf_counter() - t0)
    report["honest"] = {"false_positives_tol0": fp0, "false_positives_tol1e-3": fp3}
    # Honest noise when the auditor runs with a different thread count than the sender.
    torch.set_num_threads(4)
    for c in ctxs[:8]:
        noise.append(ca.max_cache_diff(clean(c), (torch.set_num_threads(1), clean(c))[1]))
    torch.set_num_threads(1)
    report["honest_noise_across_thread_counts_max_entry_diff"] = max(noise)
    report["detection"] = {}
    for coef in [float(x) for x in a.coefs.split(",")]:
        row = {}
        for tol in (0.0, 1e-3):
            caught = 0
            for c in ctxs:
                bad = steered(c, coef)
                rec = ca.commit(key, weights, c, bad)  # a lying sender signs its own steered cache
                caught += not ca.verify(rec, c, bad, weights, {pub}, recompute=clean, tol=tol).ok
            row[f"caught_tol{tol:g}"] = caught
        report["detection"][str(coef)] = {"n": len(ctxs), **row}
    report["cost_seconds_mean"] = {"audit_recompute": sum(t_audit) / len(t_audit), "b_reads_query_on_cache": sum(t_continue) / len(t_continue)}
    Path(a.out).write_text(json.dumps(report, indent=1))
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
