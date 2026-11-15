#!/usr/bin/env python3
"""Compare room vs silo trials (best score after k total tries) and a classical hill-climber with the same budget."""
import glob
import json
import os
import random
import statistics
import sys

B = os.environ.get("BENCH_DIR") or "bench"
DIM = int(os.environ.get("BENCH_DIM", "16"))
CP = [3, 15, 30, 60, 90, 120]
A, C = (37, 11) if DIM == 16 else (53, 29)


def score(x):
    st = sx = st2 = 0
    for i, v in enumerate(x, 1):
        t = (i * A + C) % 256
        st += (v - t) ** 2
        sx += v
        st2 += t
    return st + 2 * abs(sx - st2)


def curve(rows):
    best, out = None, []
    for r in sorted(rows, key=lambda r: float(r[0])):
        s = int(r[2])
        best = s if best is None else min(best, s)
        out.append(best)
    return out


def classical(seed, n=120):
    rnd = random.Random(seed)
    x = [128] * DIM
    best, out = score(x), []
    for k in range(n):
        if k < 3:
            out.append(best)                    # the three agents' shared start, as in the LLM runs
            continue
        y = list(x)
        for i in rnd.sample(range(DIM), rnd.randint(1, 4)):
            y[i] = min(255, max(0, y[i] + int(rnd.gauss(0, 25))))
        s = score(y)
        if s < best:
            x, best = y, s
        out.append(best)
    return out


def perm(groups):
    """Permutation test (room vs silo): difference in mean final best, and in mean best-so-far over the whole run. One-sided: room lower."""
    r, s = groups["room"], groups["silo"]
    if len(r) < 2 or len(s) < 2:
        return
    import itertools
    print("\nsignificance (permutation test, room vs silo, lower is better):")
    for label, f in (("final best", lambda c: c[-1]), ("mean best-so-far over the run", lambda c: sum(c) / len(c))):
        a = [f(c) for c, _ in r]
        b = [f(c) for c, _ in s]
        allv, n = a + b, len(a)
        obs = statistics.mean(b) - statistics.mean(a)
        ge = tot = 0
        for idx in itertools.combinations(range(len(allv)), n):
            x = [allv[i] for i in idx]
            y = [allv[i] for i in range(len(allv)) if i not in idx]
            tot += 1
            ge += (statistics.mean(y) - statistics.mean(x)) >= obs - 1e-9
        print(f"  {label}: room is {obs:,.0f} lower on average ({100 * obs / statistics.mean(b):.0f}%), p = {ge / tot:.3f}  (room {statistics.mean(a):,.0f} +- {statistics.stdev(a):,.0f}, silo {statistics.mean(b):,.0f} +- {statistics.stdev(b):,.0f})")


def main():
    res = [json.loads(ln) for ln in open(os.path.join(B, "results.jsonl"))] if os.path.exists(os.path.join(B, "results.jsonl")) else []
    groups = {"room": [], "silo": []}
    for r in res:
        if not r["complete"]:
            print(f"  (excluded {r['cond']}{r['trial']}: {r['tries']} tries, invalid={r['invalid_tries']}, {r['note']})")
            continue
        rows = [ln.split("\t") for ln in open(os.path.join(B, f"{r['cond']}{r['trial']}", "scores.tsv")).read().splitlines()]
        groups[r["cond"]].append((curve(rows), r["elapsed_s"]))
    cls = [classical(s) for s in range(20)]
    print(f"{'best score after k tries':<26}" + "".join(f"{k:>9}" for k in CP))
    for name, g in (("Playbox room (3 shared)", groups["room"]), ("silos (3 independent)", groups["silo"])):
        if g:
            print(f"{name:<26}" + "".join(f"{statistics.mean(c[k - 1] for c, _ in g):>9.0f}" for k in CP) + f"   n={len(g)} trials")
    print(f"{'classical (no LLM)':<26}" + "".join(f"{statistics.mean(c[k - 1] for c in cls):>9.0f}" for k in CP) + "   n=20 seeds")
    print("\nper trial, final best:")
    for name, g in groups.items():
        print(f"  {name}: " + ", ".join(f"{c[-1]}" for c, _ in g) + f"   (mean wall time {statistics.mean(t for _, t in g):.0f}s)" if g else f"  {name}: none")
    perm(groups)
    start = score([128] * DIM)
    THRESH = [int(start * f) for f in (0.75, 0.5, 0.25)]
    print("\ntries needed to first get below a score (mean over trials; '-' = never):")
    for name, g in (("room", groups["room"]), ("silo", groups["silo"])):
        cells = []
        for t in THRESH:
            hit = [next((i + 1 for i, v in enumerate(c) if v < t), None) for c, _ in g]
            ok = [h for h in hit if h]
            cells.append(f"<{t}: " + (f"{statistics.mean(ok):.0f} ({len(ok)}/{len(g)})" if ok else "-"))
        print(f"  {name}: " + "   ".join(cells))


if __name__ == "__main__":
    sys.exit(main())
