#!/usr/bin/env python3
"""Haiku-only ROOM arm: the three agents share a room, no advisor, many tries. Trials run in parallel (each has its own room and host).
Env: BENCH_DIR, BENCH_CAP (tries per agent), BENCH_TRIALS (parallel trials), BENCH_TIMEOUT (seconds per trial)."""
import os
import threading
os.environ.setdefault("HC_ADV_EVERY", "1000000")          # never ask the advisor
import hc_bench_adv as b  # noqa: E402

if __name__ == "__main__":
    os.makedirs(b.B, exist_ok=True)
    ts = []
    for t in range(1, b.TRIALS + 1):
        ts.append(threading.Thread(target=b.room, args=(t,)))
        ts[-1].start()
        import time
        time.sleep(20)                                   # stagger the room start-ups
    [t.join() for t in ts]
    print("ALL DONE", flush=True)
