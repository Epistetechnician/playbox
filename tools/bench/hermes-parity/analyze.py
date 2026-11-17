#!/usr/bin/env python3
"""Summarise runs/*.json: per-arm means, and a paired-free non-inferiority check on hidden-test score (margin 2 of 24)."""
import json, glob, os, statistics as st, random, sys
d = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/.playbox-hermes/openshell-parity/runs")
R = {"L": [], "S": []}
for f in sorted(glob.glob(f"{d}/[LS]*.json")):
    if f.endswith("usage.json"): continue
    r = json.load(open(f)); R[r["arm"]].append(r)
def col(a, fn): return [fn(r) for r in R[a]]
for a in "LS":
    rs = R[a]; u = lambda k: [r["usage"].get(k, 0) for r in rs]
    p = col(a, lambda r: r["passed"])
    print(f"arm {a}: runs={len(rs)} tests mean={st.mean(p):.2f}/24 reached24={sum(x==24 for x in p)} min={min(p)}"
          f" | cost mean=${st.mean(u('estimated_cost_usd')):.4f} (sum ${sum(u('estimated_cost_usd')):.3f})"
          f" | tokens mean={st.mean(u('total_tokens')):.0f} in={st.mean(u('input_tokens')):.0f} out={st.mean(u('output_tokens')):.0f}"
          f" | api_calls={st.mean(u('api_calls')):.1f} | wall mean={st.mean(col(a, lambda r: r['wall_s'])):.1f}s median={st.median(col(a, lambda r: r['wall_s'])):.1f}s")
    if a == "S": print(f"   S per-run upload+git-init overhead mean={st.mean(r.get('upload_s',0) for r in rs):.2f}s")
pl, ps = col("L", lambda r: r["passed"]), col("S", lambda r: r["passed"])
diff = st.mean(ps) - st.mean(pl)
random.seed(0); bs = sorted(st.mean(random.choices(ps, k=len(ps))) - st.mean(random.choices(pl, k=len(pl))) for _ in range(20000))
print(f"S - L mean tests = {diff:+.2f}; bootstrap 95% CI [{bs[500]:+.2f}, {bs[19500]:+.2f}]; one-sided 97.5% lower bound {bs[500]:+.2f} vs margin -2 -> {'non-inferior' if bs[500] > -2 else 'NOT shown'}")
cl, cs = col("L", lambda r: r["usage"]["estimated_cost_usd"]), col("S", lambda r: r["usage"]["estimated_cost_usd"])
print(f"cost S/L = {st.mean(cs)/st.mean(cl):.2f}x ; wall S/L = {st.mean(col('S', lambda r: r['wall_s']))/st.mean(col('L', lambda r: r['wall_s'])):.2f}x")
