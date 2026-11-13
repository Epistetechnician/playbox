#!/usr/bin/env python3
"""Recompute the README's headline research numbers from the raw result files and compare.

Reads only files in the repo (no GPU, no network). Exit status 1 if any claim does not match.
    python3 scripts/verify_claims.py
"""
import json
import math
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEL = ROOT / "steer/results/telepathy"
failed = []


def wilson(k, n, z=1.96):
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return 100 * (c - h), 100 * (c + h)


def check(name, got, want, tol=0.0):
    ok = abs(got - want) <= tol
    print(f"{'PASS' if ok else 'FAIL'}  {name}: got {got:g}, README says {want:g}")
    if not ok:
        failed.append(name)


def gated(dirs):
    t = tc = c = cc = 0
    for d in dirs:
        r = json.load(open(TEL / d / "results.json"))
        top = [x for x in r if x["truth"] != "none"]
        ctl = [x for x in r if x["truth"] == "none"]
        t += sum(x["ok"] for x in top); tc += len(top)
        c += sum(x["ok"] for x in ctl); cc += len(ctl)
    return t, tc, c, cc


# 1. Qwen3-8B <-> OLMo-2-7B, probe-gated, two seeds pooled
t, tn, c, cn = gated(["big_pair_gated_private", "big_pair_gated_s2_private"])
check("qwen8b->olmo2 gated topics correct", t, 140)
check("qwen8b->olmo2 gated trials", tn, 160)
check("qwen8b->olmo2 gated controls answered none", c, 71)
check("qwen8b->olmo2 gated controls total", cn, 72)
lo, hi = wilson(t, tn)
check("qwen8b->olmo2 gated Wilson CI low (%)", round(lo), 81)
check("qwen8b->olmo2 gated Wilson CI high (%)", round(hi), 92)

# 1b. Fresh reproduction on 2026-11-13 (seed label s3, same node type and shipped probe): 71/80 topics, 36/36 controls
t3, tn3, c3, cn3 = gated(["big_pair_gated_s3_private"])
check("s3 reproduction topics correct", t3, 71)
check("s3 reproduction controls answered none", c3, 36)
check("three-seed pooled topics correct", gated(["big_pair_gated_private", "big_pair_gated_s2_private", "big_pair_gated_s3_private"])[0], 211)

# 2. Qwen3-8B -> DeepSeek-7B, receiver at layer fraction 0.9, two seeds
t, tn, c, cn = gated(["dsk_pair_r09_s1_private", "dsk_pair_r09_s2_private"])
check("qwen8b->deepseek layer 0.9 topics correct", t, 139)
check("qwen8b->deepseek layer 0.9 controls answered none", c, 68)

# 2b. Fresh reproduction on 2026-11-13 (seed labels s3, s4; same stack, probe and controls): 141/160 topics, 68/72 controls
t, tn, c, cn = gated(["dsk_pair_r09_s3_private", "dsk_pair_r09_s4_private"])
check("deepseek repro topics correct", t, 141)
check("deepseek repro controls answered none", c, 68)
t, tn, c, cn = gated(["dsk_pair_r09_s1_private", "dsk_pair_r09_s2_private", "dsk_pair_r09_s3_private", "dsk_pair_r09_s4_private"])
check("deepseek four-seed pooled topics correct", t, 280)
check("deepseek four-seed pooled controls answered none", c, 136)

# 3. Swarm, both directions pooled (400 trials)
s = json.load(open(TEL / "swarm/summary.json"))["big pair, both directions"]["all"]
arms = s["arms"]
check("swarm trials", s["n"], 400)
check("swarm novelty recall (%)", round(100 * arms["latent_novelty"]["recall"], 1), 55.7)
check("swarm one-agent recall (%)", round(100 * arms["latent_one"]["recall"], 1), 44.5)
check("swarm all-summed recall (%)", round(100 * arms["latent_all"]["recall"], 1), 46.7)
check("swarm novelty rare found (%)", round(100 * arms["latent_novelty"]["rare"], 1), 54.2, 0.1)
check("swarm summed rare found (%)", round(100 * arms["latent_all"]["rare"], 1), 11.5)
check("swarm novelty all-k recovered (%)", round(100 * arms["latent_novelty"]["full"] / s["n"]), 17)
check("swarm text-oracle all-k recovered (%)", round(100 * arms["text_words"]["full"] / s["n"]), 66)
check("swarm sign test novelty vs summed, p < 1e-8", int(s["tests"]["latent_novelty vs latent_all"]["p"] < 1e-8), 1)

# 4. Hill-climb gate receipts: 6 signed, 4 rejected, 2 promoted (verdict field only; signatures: challenge/gate.py verify)
rec = [json.loads(l) for l in open(ROOT / "challenge/results/receipts.jsonl") if l.strip()]
check("challenge receipts", len(rec), 6)
check("challenge rejected", sum(r["body"]["verdict"] == "reject" for r in rec), 4)
check("challenge promoted", sum(r["body"]["verdict"] != "reject" for r in rec), 2)

# 5. Hub (latent-channels.md 6a): mean over 20 ordered pairs at strength 0.5
import statistics
e8 = json.load(open(ROOT / "steer/results/e8/e8.json"))
check("hub pairs", len(e8["pairs"]), 20)
for arm, want in (("hub", 0.47), ("pair", 0.51), ("direct", 0.81), ("null", 0.03)):
    check(f"e8 {arm} mean id accuracy", statistics.mean(v["0.5"][arm] for v in e8["pairs"].values()), want, 0.006)
check("e8 hub wire bytes", e8["hub"]["wire_bytes_per_thought"], 64)

# 6. KV hybrid, corrected (salted) run, 4B, pooled lookup over both corpora: hybrid does NOT beat RAG
kv = json.load(open(ROOT / "steer/results/kv_hybrid/kv_v4_4b.json"))["test"]


def pooled(arm, field):
    c = [v for k, v in kv.items() if k.endswith("/lookup/" + arm)]
    return sum(x[field] * x["n"] for x in c) / sum(x["n"] for x in c)


check("kv 4B rag turns/min", round(pooled("rag", "turns_per_min")), 4526)
check("kv 4B hybrid turns/min", round(pooled("hybrid24", "turns_per_min")), 3053)
check("kv 4B hybrid lookup accuracy (%)", round(100 * pooled("hybrid24", "acc"), 1), 96.1)
check("kv 4B hybrid slower than rag", int(pooled("hybrid24", "turns_per_min") < pooled("rag", "turns_per_min")), 1)

# 7. Near-miss liars (latent-channels.md 6r): audit, guard, payload effect
E8 = ROOT / "steer/results/e8"
nm = json.load(open(E8 / "e8_audit_nearmiss.json"))["result"]
check("nearmiss honest passed (mean-2sd)", int(nm["honest_passed"]["honest_mean_minus_2sd"] == "90/90"), 1)
check("nearmiss 25% medicine mix caught", int(nm["liars"]["mix_guarded_0.25"]["caught"]["honest_mean_minus_2sd"].split("/")[0]), 1)
check("nearmiss 50% medicine mix caught", int(nm["liars"]["mix_guarded_0.5"]["caught"]["honest_mean_minus_2sd"].split("/")[0]), 15)
check("nearmiss 75% medicine mix caught", int(nm["liars"]["mix_guarded_0.75"]["caught"]["honest_mean_minus_2sd"].split("/")[0]), 71)
gd = json.load(open(E8 / "e8_guard_nearmiss.json"))["by_mix"]
check("guard false blocks (honest)", gd["0.0"]["blocked"], 3)
check("guard blocks 25% mix", gd["0.25"]["blocked"], 29)
check("guard blocks 50% mix", gd["0.5"]["blocked"], 96)
pe = json.load(open(E8 / "e8_payload_effect.json"))["by_mix"]
check("payload at 25% mix read as medicine", pe["0.25"]["medicine"], 0)
check("payload at 50% mix read as medicine", pe["0.5"]["medicine"], 21)
check("payload at 100% read as medicine", pe["1.0"]["medicine"], 144)

print()
print("ALL CLAIMS MATCH" if not failed else f"{len(failed)} MISMATCH(ES): {failed}")
sys.exit(1 if failed else 0)
