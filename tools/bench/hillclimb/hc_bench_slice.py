#!/usr/bin/env python3
"""Benchmark: three Haiku 5.5 agents hill-climbing in a shared Playbox room vs. the same three working in silos.

Same agent code, prompt, model, task, boxes and budget (CAP tries per agent). Only the information differs: in the room each agent
sees everyone's tries; in a silo each sees only its own. Trials alternate (room, silo, room, ...) so drift in API speed hits both.
Results: bench/<cond><trial>/scores.tsv (every try, in time order) and bench/results.jsonl (one line per trial)."""
import json
import os
import shlex
import subprocess
import sys
import threading
import time

H = os.path.dirname(os.path.abspath(__file__))
B = os.environ.get("BENCH_DIR") or os.path.join(H, "bench")
sys.path.insert(0, os.path.join(H, "..", ".."))
import playbox_watch  # noqa: E402  (its Mcp client)

CAP = int(os.environ.get("BENCH_CAP", "40"))
TRIALS = int(os.environ.get("BENCH_TRIALS", "3"))
DIM = os.environ.get("BENCH_DIM", "16")
SCRIPT = "/task/score32.sh" if DIM == "32" else "/task/score.sh"
N = 3
TIMEOUT = float(os.environ.get("BENCH_TIMEOUT", "1000"))
KICKOFF = "Task: together find 32 integers (0..255) matching a hidden target. Your box scores only your own slice, so share every try as: TRY slice=<k> score=<n> vec=[32 ints]; teammates need your best slice values and you need theirs. Go."


def env_for(i, d, mode):
    return {**os.environ, "HC_BOX": f"pbhc-box{i}", "HC_STATE": os.path.join(d, f"a{i}"), "HC_CAP": str(CAP), "HC_MODE": mode,
            "HC_LOG": os.path.join(d, "scores.tsv"), "HC_SLICE": str(i)}


def tries(d):
    try:
        return [ln.split("\t") for ln in open(os.path.join(d, "scores.tsv")).read().splitlines()]
    except OSError:
        return []


def record(cond, trial, d, t0, note=""):
    rows = tries(d)
    bad = sum(1 for r in rows if r[4] not in ("haiku", "start"))
    res = {"cond": cond, "trial": trial, "tries": len(rows), "invalid_tries": bad, "elapsed_s": round(time.time() - t0, 1),
           "best": min((int(r[2]) for r in rows), default=None), "complete": len(rows) >= N * CAP and bad == 0, "note": note}
    with open(os.path.join(B, "results.jsonl"), "a") as f:
        f.write(json.dumps(res) + "\n")
    print(json.dumps(res), flush=True)


def silo(trial):
    d = os.path.join(B, f"silo{trial}")
    os.makedirs(d)
    t0, fail = time.time(), []

    def agent(i):
        for _ in range(CAP):
            r = subprocess.run([sys.executable, os.path.join(H, "hc_agent_slice.py")], input=json.dumps({"name": f"Haiku-{i}", "transcript": [], "new": []}),
                               capture_output=True, text=True, env=env_for(i, d, "silo"), timeout=300)
            if r.returncode != 0:
                fail.append(f"agent {i}: {(r.stderr or r.stdout).strip()[-160:]}")
                return
    ts = [threading.Thread(target=agent, args=(i,)) for i in range(1, N + 1)]
    [t.start() for t in ts]
    [t.join() for t in ts]
    record("silo", trial, d, t0, "; ".join(fail))


def room(trial):
    d = os.path.join(B, f"room{trial}")
    os.makedirs(d)
    t0, note = time.time(), ""
    penv = {**os.environ, "PLAYBOX_HOME": os.path.join(d, "home"), "PLAYBOX_HOSTING_DIR": os.path.join(B, "hosting")}
    host = subprocess.Popen(["playbox", "up", "--home", os.path.join(d, "home"), "--local", "--console", "--state", os.path.join(d, "state")],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=penv)
    links = []

    def pump():
        for ln in host.stdout:
            for w in ln.split():
                if w.startswith("playbox://"):
                    links.append(w)
    threading.Thread(target=pump, daemon=True).start()
    for _ in range(60):
        if links:
            break
        time.sleep(0.5)
    for _ in range(N):                                   # the first link was printed at startup: one per agent, one for this driver
        host.stdin.write("link\n")
        host.stdin.flush()
        time.sleep(1.5)
    watchers, stop = [], os.path.join(d, "stop")
    me = None
    try:
        if len(links) < N + 1:
            raise RuntimeError(f"only {len(links)} links")
        for i in range(1, N + 1):
            lf = os.path.join(d, f"{i}.link")
            fd = os.open(lf, os.O_WRONLY | os.O_CREAT, 0o600)
            os.write(fd, links[i - 1].encode())
            os.close(fd)
            watchers.append(subprocess.Popen(
                [sys.executable, os.path.join(H, "..", "..", "playbox_watch.py"), "--link-file", lf, "--home", os.path.join(d, f"w{i}"),
                 "--name", f"Haiku-{i}", "--role", f"You are agent {i} of 3 hill-climbing together in a Playbox room; share every try so the others can build on it.",
                 "--responder", f"{sys.executable} {os.path.join(H, 'hc_agent_slice.py')}", "--wait", "15", "--cooldown", "3", "--max-per-hour", "1000",
                 "--max-streak", "100000", "--history", os.environ.get("BENCH_HISTORY", "30"), "--duration", str(TIMEOUT + 60), "--stop-file", stop, "--log", os.path.join(d, f"w{i}.log")],
                env=env_for(i, d, "room"), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            time.sleep(5)                                # the host refuses simultaneous joins ("busy")
        me = playbox_watch.Mcp(["playbox", "mcp"], os.path.join(d, "me"))
        me.call("playbox_join", {"link": links[N]})
        me.call("playbox_say", {"name": "bench", "text": KICKOFF})
        end = time.time() + TIMEOUT
        while time.time() < end and len(tries(d)) < N * CAP:
            if all(w.poll() is not None for w in watchers):
                note = "all watchers exited early"
                break
            time.sleep(3)
        if len(tries(d)) < N * CAP and not note:
            note = "timed out"
    except Exception as e:  # noqa: BLE001
        note = f"driver error: {e}"
    finally:
        open(stop, "w").close()
        time.sleep(3)
        for w in watchers:
            w.kill()
        if me:
            me.close()
        try:
            host.stdin.write("quit\n")
            host.stdin.flush()
            host.wait(timeout=10)
        except Exception:  # noqa: BLE001
            host.kill()
    record("room", trial, d, t0, note)


def main():
    os.makedirs(B, exist_ok=True)
    for t in range(1, TRIALS + 1):
        for cond, fn in (("room", room), ("silo", silo)):
            if os.path.isdir(os.path.join(B, f"{cond}{t}")):
                continue
            for attempt in range(1, 9):
                print(f"{time.strftime('%H:%M:%S')} start {cond}{t} (attempt {attempt})", flush=True)
                fn(t)
                last = json.loads(open(os.path.join(B, "results.jsonl")).read().splitlines()[-1])
                if last["complete"]:
                    break
                failed = os.path.join(B, f"failed-{cond}{t}-{attempt}")
                os.rename(os.path.join(B, f"{cond}{t}"), failed)
                print(f"  incomplete ({last['note'] or last['tries']}); waiting 10 min, then retrying", flush=True)
                time.sleep(600)
    print("ALL DONE", flush=True)


if __name__ == "__main__":
    main()
