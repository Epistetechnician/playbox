#!/usr/bin/env python3
"""One hill-climbing agent: asks Haiku 5.5 for an improved candidate, scores it in its own Docker box, posts one line.

Used two ways with the same code and prompt:
  * as the responder of tools/playbox_watch.py (HC_MODE=room): it sees what the room has said;
  * driven by the silo runner (HC_MODE=silo): it sees only its own earlier tries.
Environment: HC_BOX (container), HC_MODEL, HC_STATE (dir for this agent's counter/history), HC_CAP (max tries),
HC_MODE (room|silo), HC_LOG (tsv of every try), HC_STRICT=1 (never fall back to random steps; fail instead)."""
import json
import os
import random
import re
import subprocess
import sys
import time

BOX = os.environ["HC_BOX"]
MODEL = os.environ.get("HC_MODEL", "claude-haiku-5-5")
MODE = os.environ.get("HC_MODE", "room")
STRICT = os.environ.get("HC_STRICT") == "1"
CAP = int(os.environ.get("HC_CAP", "0"))
DIM = int(os.environ.get("HC_DIM", "16"))
SCRIPT = os.environ.get("HC_SCRIPT", "/task/score.sh")
STATE = os.environ.get("HC_STATE")
HERE = os.path.dirname(os.path.abspath(__file__))
LOG = os.environ.get("HC_LOG") or os.path.join(HERE, "logs", "scores.tsv")
TRY = re.compile(r"score=(\d+) vec=\[([0-9, ]+)\]")


def parse(lines):
    out = []
    for ln in lines:
        for m in TRY.finditer(ln):
            v = [int(x) for x in m.group(2).replace(" ", "").split(",") if x]
            if len(v) == DIM:
                out.append((int(m.group(1)), v, ln.split(":", 1)[0]))
    return out


def score(vec):
    r = subprocess.run(["docker", "exec", BOX, SCRIPT, *map(str, vec)], capture_output=True, text=True, timeout=30)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:100])
    return int(r.stdout.strip())


def ask(best, recent, name):
    p = (
        f"You are {name}, one of three agents hill-climbing together. Find {DIM} integers (each 0..255) that minimise a hidden score; "
        "0 is perfect. The score is the squared distance to a hidden target vector, plus 2x the absolute difference of the sums. "
        f"Best so far: score={best[0]} vec={best[1]}.\nRecent tries (score, vec):\n"
        + "\n".join(f"{s} {v}" for s, v, _ in recent)
        + "\nCompare tries to infer which coordinates moved the score the right way, then propose ONE new vector that should beat the best "
        f"(change several coordinates, by larger steps when far away and smaller when close). Reply with only a JSON array of {DIM} integers."
    )
    r = subprocess.run(["claude", "-p", "--model", MODEL, "--tools", "", "--no-session-persistence", "--disable-slash-commands"],
                       input=p, capture_output=True, text=True, timeout=120)
    m = re.search(r"\[[0-9,\s]+\]", r.stdout)
    if not m:
        if STRICT:
            raise RuntimeError("haiku gave no vector: " + (r.stdout or r.stderr).strip()[:120])
        return None
    v = json.loads(m.group(0))
    if len(v) == DIM and all(isinstance(x, int) and 0 <= x <= 255 for x in v):
        return v
    if STRICT:
        raise RuntimeError("haiku gave an invalid vector")
    return None


def load_state():
    if STATE:
        try:
            return json.load(open(os.path.join(STATE, "state.json")))
        except (OSError, ValueError):
            pass
    return {"n": 0, "hist": []}


def save_state(st):
    if STATE:
        os.makedirs(STATE, exist_ok=True)
        json.dump(st, open(os.path.join(STATE, "state.json"), "w"))


def main():
    d = json.load(sys.stdin)
    name = d["name"]
    st = load_state()
    if CAP and st["n"] >= CAP:
        print("NOOP")
        return
    seen = [(s, v, name) for s, v in st["hist"]] if MODE == "silo" else parse(d["transcript"] + d["new"])
    if seen and min(s for s, _, _ in seen) == 0:
        print("NOOP")                                  # solved: stay quiet
        return
    if not seen:
        vec, how = [128] * DIM, "start"
    else:
        best = min(seen, key=lambda t: t[0])
        recent = sorted({(s, tuple(v)): (s, v, w) for s, v, w in seen}.values(), key=lambda t: t[0])[:10]
        vec, how = ask(best, recent, name), "haiku"
        if vec is None:                                # Haiku gave nothing usable: perturb the best, and say so
            rng = random.Random(f"{name}:{len(seen)}")  # seeded per agent and history length, so a rerun reproduces the fallback
            vec = [min(255, max(0, x + rng.randint(-12, 12))) for x in best[1]]
            how = "fallback"
    s = score(vec)
    st["n"] += 1
    st["hist"].append([s, vec])
    save_state(st)
    top = min([(s, vec)] + [(t[0], t[1]) for t in seen], key=lambda t: t[0])
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    with open(LOG, "a") as f:
        f.write(f"{time.time():.3f}\t{name}\t{s}\t{top[0]}\t{how}\n")
    print(f"TRY score={s} vec={json.dumps(vec)} | BEST score={top[0]} vec={json.dumps(top[1])} via={how} box={BOX}")


if __name__ == "__main__":
    main()
