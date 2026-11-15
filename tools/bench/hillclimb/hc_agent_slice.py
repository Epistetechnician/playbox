#!/usr/bin/env python3
"""Sliced task: 32 integers, but each agent's box can only score ITS slice of 11/11/10 coordinates. Solving the whole needs all three slices' knowledge.
room: sees every teammate's tries (each tagged slice=k) and assembles the best known value of each slice. silo: sees only its own tries; other slices stay 128.
Env: HC_BOX, HC_SLICE (1..3), HC_MODE, HC_STATE, HC_CAP, HC_LOG, HC_MODEL."""
import json, os, re, subprocess, sys, time
BOX = os.environ["HC_BOX"]; K = int(os.environ["HC_SLICE"]); MODE = os.environ.get("HC_MODE", "room")
MODEL = os.environ.get("HC_MODEL", "claude-haiku-5-5"); CAP = int(os.environ.get("HC_CAP", "0")); STATE = os.environ.get("HC_STATE")
LOG = os.environ["HC_LOG"]
SL = {1: (0, 11), 2: (11, 22), 3: (22, 32)}
A, B = SL[K]
TRY = re.compile(r"slice=(\d) score=(\d+) vec=\[([0-9, ]+)\]")
def full(v): return sum((x - ((i * 53 + 29) % 256)) ** 2 for i, x in enumerate(v, 1))
def parse(lines):
    out = []
    for ln in lines:
        for m in TRY.finditer(ln):
            v = [int(x) for x in m.group(3).replace(" ", "").split(",") if x]
            if len(v) == 32: out.append((int(m.group(1)), int(m.group(2)), v))
    return out
def score(vals):
    r = subprocess.run(["docker", "exec", BOX, "/task/slice32.sh", str(K), *map(str, vals)], capture_output=True, text=True, timeout=30)
    if r.returncode: raise RuntimeError(r.stderr.strip()[:100])
    return int(r.stdout.strip())
def ask(best, recent, name):
    n = B - A
    p = (f"You are {name}, one of three agents working together. Together you must find 32 integers (0..255) matching a hidden target. The work is split: "
         f"your box can only score coordinates {A+1}..{B} ({n} integers); teammates' boxes score the other coordinates. Score = squared distance to the hidden target on your slice; 0 is perfect.\n"
         f"Your slice, best so far: score={best[0]} values={best[1]}.\nRecent tries on your slice (score, values), best first:\n" + "\n".join(f"{s} {v}" for s, v in recent)
         + f"\nCompare tries to infer which coordinates moved the score the right way, then propose ONE new set of {n} integers that should beat the best (larger steps when far, smaller when close). Reply with only a JSON array of {n} integers.")
    r = subprocess.run(["claude", "-p", "--model", MODEL, "--tools", "", "--no-session-persistence", "--disable-slash-commands"], input=p, capture_output=True, text=True, timeout=120)
    m = re.search(r"\[[0-9,\s]+\]", r.stdout)
    if not m: raise RuntimeError("haiku gave no vector: " + (r.stdout or r.stderr).strip()[:120])
    v = json.loads(m.group(0))
    if len(v) == n and all(isinstance(x, int) and 0 <= x <= 255 for x in v): return v
    raise RuntimeError("haiku gave an invalid vector")
def main():
    d = json.load(sys.stdin); name = d["name"]
    try: st = json.load(open(os.path.join(STATE, "state.json")))
    except (OSError, ValueError, TypeError): st = {"n": 0, "hist": []}
    if CAP and st["n"] >= CAP: print("NOOP"); return
    seen = parse(d["transcript"] + d["new"]) if MODE == "room" else []
    base = [128] * 32                                     # the vector we post: best known value of every slice
    own = [(s, v[A:B]) for s, v in st["hist"]] if MODE == "silo" else [(s, v[A:B]) for k, s, v in seen if k == K]
    if MODE == "room":
        for k in (1, 2, 3):
            ks = [(s, v) for kk, s, v in seen if kk == k]
            if ks and k != K:
                a, b = SL[k]; base[a:b] = min(ks, key=lambda t: t[0])[1][a:b]
    if not own: vals, how = [128] * (B - A), "start"
    else:
        best = min(own, key=lambda t: t[0])
        recent = sorted({(s, tuple(v)): (s, v) for s, v in own}.values(), key=lambda t: t[0])[:10]
        vals, how = ask(best, recent, name), "haiku"
    vec = list(base); vec[A:B] = vals
    s = score(vals)
    st["n"] += 1; st["hist"].append([s, vec]); os.makedirs(STATE, exist_ok=True); json.dump(st, open(os.path.join(STATE, "state.json"), "w"))
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    with open(LOG, "a") as f: f.write(f"{time.time():.3f}\t{name}\t{full(vec)}\t{s}\t{how}\t{K}\n")
    print(f"TRY slice={K} score={s} vec={json.dumps(vec)} | via={how} box={BOX}")
if __name__ == "__main__": main()
