#!/usr/bin/env python3
"""Morning report for the overnight soak: python3 soak_report.py [SOAK_DIR]"""
import collections, glob, json, os, sys
D = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/.playbox-soak/run")
def jl(p):
    try: return [json.loads(l) for l in open(p) if l.strip()]
    except OSError: return []
print("== chaos rounds ==")
c = jl(f"{D}/chaos.jsonl"); by = collections.defaultdict(lambda: [0, 0])
for r in c: by[r["scenario"]][0 if r["pass"] else 1] += 1
for k, (p, f) in by.items(): print(f"  {k:<10} pass {p:>3}  fail {f:>3}")
for r in c:
    if not r["pass"]: print("  FAIL", r["at"], r["scenario"], r["detail"][:200])
print("== test sweeps ==")
for r in jl(f"{D}/sweep.jsonl"): print(" ", r)
print("== rooms (hill-climb soak) ==")
for r in jl(f"{D}/rooms/results.jsonl"): print(" ", {k: r[k] for k in ("cond", "trial", "tries", "invalid_tries", "best", "elapsed_s", "complete", "note")})
for d in sorted(glob.glob(f"{D}/rooms/room[0-9]*")):
    try: rows = [l.split("\t") for l in open(f"{d}/scores.tsv").read().splitlines()]
    except OSError: continue
    print(f"  {os.path.basename(d)}: {len(rows)} tries, best posted {min((int(r[2]) for r in rows), default=None)}")
