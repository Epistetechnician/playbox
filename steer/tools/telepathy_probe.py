"""Fit the sender-side gate on live Agent A text: a ridge-logistic probe on the unit difference vector, topic vs not.

    python tools/telepathy_probe.py LOG.jsonl calib_jobs.json OUT_PROBE.json OUT_REPORT.json

Grouped cross-validation (a whole topic / scene / subject is held out together) picks the penalty and the ramp thresholds,
so the thresholds are never set on text the probe was fit on."""
import json, sys
import numpy as np

log, jobs, out_probe, out_report = sys.argv[1:5]
meta = {j["tag"]: j for j in json.load(open(jobs)) if j.get("ok")}
rows = [json.loads(l) for l in open(log)]
by = {}
for r in rows:  # a retried command logs twice; keep the last call per tag
    if r["tag"] in meta:
        by[r["tag"]] = r


def fit(X, y, lam, it=60):
    w, b = np.zeros(X.shape[1]), 0.0
    for _ in range(it):  # Newton / IRLS
        p = 1 / (1 + np.exp(-(X @ w + b)))
        s = p * (1 - p) + 1e-6
        Xb = np.c_[X, np.ones(len(X))]
        H = Xb.T @ (Xb * s[:, None]) + lam * np.diag([1.0] * X.shape[1] + [0.0])
        g = Xb.T @ (p - y) + lam * np.r_[w, 0.0]
        step = np.linalg.solve(H, g)
        w, b = w - step[:-1], b - step[-1]
        if np.abs(step).max() < 1e-6:
            break
    return w, b


def auc(p, y):
    pos, neg = p[y == 1], p[y == 0]
    return float((pos[:, None] > neg[None, :]).mean() + 0.5 * (pos[:, None] == neg[None, :]).mean())


report = {}
probe = {}
import os
for src in os.environ.get("TELEPATHY_PAIR", "smollm2,qwen3").split(","):
    tags = [t for t, r in by.items() if r["src"] == src]
    X = np.array([by[t]["d"] for t in tags])
    X /= np.linalg.norm(X, axis=1, keepdims=True)
    y = np.array([1 if meta[t]["kind"] == "topic" else 0 for t in tags])
    # the 20 topics are the fixed vocabulary, so topic samples are held out one by one; negatives are held out by whole scene / subject
    grp = np.array([t if meta[t]["kind"] == "topic" else meta[t]["kind"] + ":" + meta[t]["subject"] for t in tags])
    mu, sd = X.mean(0), X.std(0) + 1e-6
    Z = (X - mu) / sd
    ug = sorted(set(grp))
    folds = [ug[i::8] for i in range(8)]  # 8 grouped folds

    def oof(lam):
        p = np.zeros(len(y))
        for f in folds:
            te = np.isin(grp, f)
            w, b = fit(Z[~te], y[~te], lam)
            p[te] = 1 / (1 + np.exp(-(Z[te] @ w + b)))
        return p

    grid = {lam: oof(lam) for lam in (1, 10, 100, 1000, 10000)}
    scores = {lam: auc(p, y) for lam, p in grid.items()}
    lam = max(scores, key=scores.get)
    p = grid[lam]
    lo = float(np.quantile(p[y == 0], 0.9))              # 90% of held-out negatives sit below the ramp
    hi = float(max(lo + 0.1, np.quantile(p[y == 1], 0.25)))  # 75% of held-out topics are fully on
    g = np.clip((p - lo) / (hi - lo), 0, 1)
    kinds = {k: [bool(g[i] >= 0.3) for i in range(len(y)) if meta[tags[i]]["kind"] == k] for k in ("topic", "filler", "offlist", "triple")}
    w, b = fit(Z, y, lam)
    probe[src] = {"mu": mu.tolist(), "w": (w / sd).tolist(), "b": float(b), "lo": lo, "hi": hi}
    report[src] = {"n": len(y), "lambda": lam, "cv_auc": scores, "lo": lo, "hi": hi,
                   "heldout_pass_rate": {k: [sum(v), len(v)] for k, v in kinds.items()}}
    print(src, json.dumps(report[src]))
json.dump(probe, open(out_probe, "w"))
json.dump(report, open(out_report, "w"), indent=1)
