#!/usr/bin/env python3
"""Resume silo agents that stopped short (for example on one malformed model reply). Each agent keeps its own counter in <trial>/a<i>/state.json,
so this just calls it again until it reaches CAP, retrying errors. Env: BENCH_DIR, BENCH_CAP, and the trial directory names as arguments."""
import json, os, subprocess, sys, threading, time
os.environ.setdefault("HC_ADV_EVERY", "1000000")
import hc_bench_adv as b  # noqa: E402


def drive(d, i):
    st = os.path.join(d, f"a{i}", "state.json")
    fails = 0
    while True:
        n = json.load(open(st))["n"] if os.path.exists(st) else 0
        if n >= b.CAP or fails >= 20:
            return
        r = subprocess.run([sys.executable, os.path.join(b.H, "hc_agent_adv.py")], input=json.dumps({"name": f"Haiku-{i}", "transcript": [], "new": []}),
                           capture_output=True, text=True, env=b.env_for(i, d, "silo"), timeout=300)
        fails = fails + 1 if r.returncode else 0
        if r.returncode:
            time.sleep(2)


if __name__ == "__main__":
    ts = []
    for name in sys.argv[1:]:
        d = os.path.join(b.B, name)
        for i in (1, 2, 3):
            st = os.path.join(d, f"a{i}", "state.json")
            if not os.path.exists(st) or json.load(open(st))["n"] < b.CAP:
                ts.append(threading.Thread(target=drive, args=(d, i)))
    [t.start() for t in ts]
    [t.join() for t in ts]
    for name in sys.argv[1:]:
        rows = [ln.split("\t") for ln in open(os.path.join(b.B, name, "scores.tsv")).read().splitlines()]
        bad = sum(1 for r in rows if r[4] not in ("haiku", "start", "advisor"))
        print(name, len(rows), "tries, invalid", bad, flush=True)
