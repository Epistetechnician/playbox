"""End to end on a real model: honest and cheating senders against a receiver that audits a fraction of handoffs.

    python -m playbox_steer.handoff_e2e --weights DIR --out report.json [--layer N] [--p 0.1,0.25] [--n 6] [--trials 100]

The cheater steers its context with the sentiment direction at strength `--coef`, signs the steered cache with its
own valid key, and sends it as if it were the clean cache of the text. Reported: honest handoffs accepted (must be
all), the observed fraction of cheaters quarantined within n handoffs against 1-(1-p)^n, message size, and the
time to pack, unpack and audit one handoff on this machine.
"""
from __future__ import annotations

import argparse
import json
import random
import time
from pathlib import Path
from types import SimpleNamespace

import torch
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from . import cache_audit as ca
from . import engine, handoff, latent
from .e6 import NEGATIVE, POSITIVE, contexts


def main(argv=None) -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--weights", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--layer", type=int, default=4)
    ap.add_argument("--coef", type=float, default=0.5)
    ap.add_argument("--p", default="0.1,0.25,0.5")
    ap.add_argument("--n", type=int, default=6)
    ap.add_argument("--trials", type=int, default=100)
    ap.add_argument("--contexts", type=int, default=16)
    a = ap.parse_args(argv)
    w = Path(a.weights)
    model, tok = engine.load(w)
    key = Ed25519PrivateKey.generate()
    pub = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    weights = "00" * 32
    ctxs = contexts(a.contexts)
    clean = lambda t: latent.read_context(model, tok, t, None)
    direction = torch.tensor(engine.extract_diffmean(w, POSITIVE, NEGATIVE, "resid_post", a.layer))
    spec = latent._spec(a.layer, a.coef, "prompt")
    norm = engine.calibrate(model, tok, spec, SimpleNamespace(prompts=tuple(ctxs)))

    def cheat(text):
        with engine.Steerer(model, spec, direction, norm):
            return latent.read_context(model, tok, text, None)

    honest = [handoff.pack(key, weights, c, clean(c)) for c in ctxs]
    bad = [handoff.pack(key, weights, c, cheat(c)) for c in ctxs]
    report = {"model": str(w), "layer": a.layer, "coef": a.coef, "contexts": len(ctxs), "n_per_trial": a.n, "trials": a.trials,
              "message_bytes_mean": sum(map(len, honest)) / len(honest)}

    t0 = time.perf_counter(); [handoff.pack(key, weights, c, clean(c)) for c in ctxs]; t_pack = (time.perf_counter() - t0) / len(ctxs)
    t0 = time.perf_counter(); [handoff.unpack(m) for m in honest]; t_unpack = (time.perf_counter() - t0) / len(ctxs)
    r = handoff.Receiver(weights, {pub}, clean, audit_prob=1.0)
    t0 = time.perf_counter(); res = [r.accept(m)[0] for m in honest]; t_audit = (time.perf_counter() - t0) / len(ctxs)
    report["honest_accepted_with_full_audit"] = sum(d.accepted for d in res)
    report["seconds_per_handoff"] = {"pack_incl_recompute": t_pack, "unpack": t_unpack, "accept_with_audit": t_audit}
    report["cheaters_caught_when_always_audited"] = sum(not handoff.Receiver(weights, {pub}, clean, audit_prob=1.0).accept(m)[0].accepted for m in bad)

    rng = random.Random(0)
    report["by_p"] = {}
    for p in [float(x) for x in a.p.split(",")]:
        caught = honest_refused = 0
        first = []
        for t in range(a.trials):
            rc = handoff.Receiver(weights, {pub}, clean, audit_prob=p, draw=rng.random)
            for i in range(a.n):
                if not rc.accept(bad[(t + i) % len(bad)])[0].accepted:
                    first.append(i + 1)
                    break
            caught += pub in rc.quarantined
            rh = handoff.Receiver(weights, {pub}, clean, audit_prob=p, draw=rng.random)
            honest_refused += sum(not rh.accept(honest[(t + i) % len(honest)])[0].accepted for i in range(a.n))
        report["by_p"][str(p)] = {"observed_caught_within_n": caught / a.trials, "predicted": ca.detection_probability(p, a.n),
                                  "mean_handoffs_until_caught": sum(first) / len(first) if first else None, "honest_refused": honest_refused}
    Path(a.out).write_text(json.dumps(report, indent=1))
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
