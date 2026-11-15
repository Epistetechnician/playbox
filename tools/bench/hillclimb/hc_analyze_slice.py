#!/usr/bin/env python3
"""Sliced task: score = sum over the 3 slices of the best slice score found so far (what a team could assemble). Room vs silo, permutation test."""
import itertools, json, os, statistics, sys
B = os.environ.get("BENCH_DIR") or "bench"
CP = [3, 15, 30, 60, 90]
START = None


def curve(rows):
    best, out = {1: None, 2: None, 3: None}, []
    for r in sorted(rows, key=lambda r: float(r[0])):
        k, s = int(r[5]), int(r[3])
        best[k] = s if best[k] is None else min(best[k], s)
        out.append(sum(v for v in best.values() if v is not None) if all(v is not None for v in best.values()) else None)
    return out


def posted(rows):
    """Best full vector actually posted (room agents post the assembled vector; silo agents can't)."""
    b, out = None, []
    for r in sorted(rows, key=lambda r: float(r[0])):
        b = int(r[2]) if b is None else min(b, int(r[2]))
        out.append(b)
    return out


res = [json.loads(ln) for ln in open(os.path.join(B, "results.jsonl"))]
g = {"room": [], "silo": []}
for r in res:
    if not r["complete"]:
        print(f"  (excluded {r['cond']}{r['trial']}: {r['tries']} tries, {r['note']})")
        continue
    rows = [ln.split("\t") for ln in open(os.path.join(B, f"{r['cond']}{r['trial']}", "scores.tsv")).read().splitlines()]
    g[r["cond"]].append((curve(rows), posted(rows), r["elapsed_s"]))
print(f"{'best assembled score after k tries':<36}" + "".join(f"{k:>9}" for k in CP))
for n, key in (("room", "room"), ("silo", "silo")):
    if g[key]:
        print(f"{n:<36}" + "".join(f"{statistics.mean(c[min(k, len(c)) - 1] for c, _, _ in g[key] if c[min(k, len(c)) - 1] is not None):>9.0f}" for k in CP) + f"   n={len(g[key])}")
print("\nbest vector actually posted (final): " + "; ".join(f"{n}: {statistics.mean(p[-1] for _, p, _ in g[n]):,.0f}" for n in g if g[n]))
if len(g["room"]) > 1 and len(g["silo"]) > 1:
    a, b = [c[-1] for c, _, _ in g["room"]], [c[-1] for c, _, _ in g["silo"]]
    allv, n = a + b, len(a)
    obs = statistics.mean(b) - statistics.mean(a)
    ge = tot = 0
    for idx in itertools.combinations(range(len(allv)), n):
        x = [allv[i] for i in idx]; y = [allv[i] for i in range(len(allv)) if i not in idx]
        tot += 1; ge += statistics.mean(y) - statistics.mean(x) >= obs - 1e-9
    print(f"final assembled: room {statistics.mean(a):,.0f} vs silo {statistics.mean(b):,.0f}; room lower by {obs:,.0f} ({100*obs/statistics.mean(b):.0f}%), p = {ge/tot:.3f}")
