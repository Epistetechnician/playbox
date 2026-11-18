#!/usr/bin/env python3
"""Paired analysis of swe-results2: pair L{i} with S{i} on the same instance (same run index, run back to back).
usage: analyze_paired.py [DIR]"""
import json, glob, os, random, sys, statistics as st
from math import comb
D = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "swe-results2")
def rd(inst, a, i):
    r = json.load(open(f"{D}/django-{inst}-{a}{i}.json"))
    return r["resolved"], r["wall_s"], (r.get("usage") or {}).get("estimated_cost_usd")
pairs = []   # (inst, i, rL, rS, wallL, wallS, costL, costS)
for inst in ("11848", "11422", "11905"):
    for i in range(1, 16):
        if os.path.exists(f"{D}/django-{inst}-L{i}.json") and os.path.exists(f"{D}/django-{inst}-S{i}.json"):
            l, s = rd(inst, "L", i), rd(inst, "S", i)
            pairs.append((inst, i, l[0], s[0], l[1], s[1], l[2], s[2]))
def mcnemar(b, c):   # exact two-sided; b = S only, c = L only
    n = b + c
    if n == 0: return 1.0
    k = min(b, c); p = sum(comb(n, j) for j in range(k + 1)) / 2**n
    return min(1.0, 2 * p)
def report(name, ps):
    b = sum(1 for p in ps if p[3] and not p[2]); c = sum(1 for p in ps if p[2] and not p[3])
    both = sum(1 for p in ps if p[2] and p[3]); neither = sum(1 for p in ps if not p[2] and not p[3])
    n = len(ps); d = (b - c) / n
    random.seed(1); bs = sorted(sum((x[3] - x[2]) for x in random.choices(ps, k=n)) / n for _ in range(10000))
    print(f"{name}: n={n} pairs  both={both} neither={neither} S-only={b} L-only={c}  diff(S-L)={d:+.3f}  paired 95% CI [{bs[250]:+.3f}, {bs[9750]:+.3f}]  McNemar exact p={mcnemar(b, c):.3f}")
    return ps
for inst in ("11848", "11422", "11905"): report(f"django-{inst}", [p for p in pairs if p[0] == inst])
allp = report("all 3 instances", pairs)
report("11848+11422", [p for p in pairs if p[0] != "11905"])
dw = [p[5] - p[4] for p in pairs]; dc = [p[7] - p[6] for p in pairs if p[6] is not None and p[7] is not None]
def ci(x):
    random.seed(2); m = sorted(st.mean(random.choices(x, k=len(x))) for _ in range(10000)); return m[250], m[9750]
print(f"paired wall diff S-L: mean {st.mean(dw):+.1f}s  95% CI [{ci(dw)[0]:+.1f}, {ci(dw)[1]:+.1f}]")
print(f"paired cost diff S-L: mean {st.mean(dc):+.4f}$ 95% CI [{ci(dc)[0]:+.4f}, {ci(dc)[1]:+.4f}]")
