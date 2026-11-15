#!/usr/bin/env python3
"""Equal-cost arm: the same three silo agents as hc_bench_adv.py silo, but Haiku only (no advisor) and many more tries, so a trial costs about
as much as an advisor trial. Several trials run in parallel. Env: BENCH_DIR, BENCH_CAP (tries per agent), BENCH_TRIALS (parallel trials)."""
import os
import threading
os.environ.setdefault("HC_ADV_EVERY", "1000000")          # never ask the advisor
import hc_bench_adv as b  # noqa: E402

if __name__ == "__main__":
    os.makedirs(b.B, exist_ok=True)
    ts = [threading.Thread(target=b.silo, args=(t,)) for t in range(1, b.TRIALS + 1)]
    [t.start() for t in ts]
    [t.join() for t in ts]
    print("ALL DONE", flush=True)
