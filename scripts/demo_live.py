#!/usr/bin/env python3
"""The live half of the demo: a hill-climb run through a real room.

    python3 scripts/demo_live.py [--playbox PATH] [--replay] [--keep DIR]

A host opens a room on a repository whose `candidate.py` is the F3 baseline and runs the gate (challenge/f3/room_gate.py) next to
it. Each guest is its own identity with its own home. A guest pulls the frontier, runs a proposal as a sandboxed task on the host,
commits the patch it gets back and pushes. The gate scores the push with the F3 harness against the frontier and the sealed
holdout, signs a receipt, and fast-forwards main only on a promotion. At the end the holdout is revealed and the receipts are
verified by challenge/f3/verify.py, which does not import the harness.

The proposals are scripted edits, not agents: what is live is the path (room, sandbox, push, gate, receipt), not the quality of
the proposals. Needs Docker (the sandbox), git, python3 with `cryptography`, `numpy` and `torch`.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
F3 = ROOT / "challenge" / "f3"
GIT = ["git", "-c", "user.email=guest@example.invalid", "-c", "user.name=guest"]


def say(msg: str) -> None:
    print(f"\n\033[1m== {msg}\033[0m", flush=True)


def ok(msg: str) -> None:
    print(f"   ok: {msg}", flush=True)


def die(msg: str) -> "None":
    print(f"   FAILED: {msg}", file=sys.stderr, flush=True)
    sys.exit(1)


def run(*cmd, cwd=None, check=True, **kw) -> subprocess.CompletedProcess:
    r = subprocess.run([str(c) for c in cmd], cwd=cwd, capture_output=True, text=True, **kw)
    if check and r.returncode != 0:
        die(f"{' '.join(map(str, cmd))[:120]}: {(r.stdout + r.stderr).strip()[-400:]}")
    return r


class Guest:
    """One member: its own home and key, driven the way an agent drives it, through `playbox mcp`."""

    def __init__(self, pb: str, home: Path, ws: Path):
        self.ws = ws
        self.p = subprocess.Popen([pb, "mcp", "--home", str(home)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.n = 0
        self._send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "demo-guest", "version": "0"}}})
        self.p.stdout.readline()
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def call(self, tool: str, **args) -> dict:
        self.n += 1
        self._send({"jsonrpc": "2.0", "id": self.n, "method": "tools/call", "params": {"name": f"playbox_{tool}", "arguments": args}})
        reply = json.loads(self.p.stdout.readline())
        res = reply.get("result", {})
        text = (res.get("content") or [{}])[0].get("text", "")
        if reply.get("error") or res.get("isError"):
            die(f"{tool}: {text or reply.get('error')}")
        try:
            return json.loads(text)
        except ValueError:
            return {"text": text}

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def sed_set(name: str, value: str) -> str:
    return f"sed -i 's/^{name} = .*/{name} = {value}/' candidate.py"


def sed_cheat() -> str:
    pad = " " * 8
    new = "\\n".join([
        "import numpy as np",
        "_i = np.random.randint(0, len(ctx.dev_blocks), BATCH)",
        "_b = torch.from_numpy(ctx.dev_blocks[_i]).long()",
        "ctx.batches += 1",
        "ctx.tokens += BATCH * ctx.block_size",
        "x, y = _b[:, :-1], _b[:, 1:]",
    ]).replace("\\n", "\\n" + pad)
    return f"sed -i 's|^{pad}x, y = ctx.next_batch(BATCH)$|{pad}{new}|' candidate.py"


# (label, the sandboxed command that makes the change, expected outcome)
PROPOSALS = [
    ("lr_3e-3", sed_set("LR", "3e-3"), "promote"),
    ("trains_on_the_dev_set", sed_cheat(), "holdout_failed"),
    ("lr_6e-3", sed_set("LR", "6e-3"), "promote"),
    ("lr_3e-2", sed_set("LR", "3e-2"), "gain_below_threshold"),
    ("touches_the_harness", "echo '# the gate is mine now' >> harness.py; " + sed_set("LR", "1e-2"), "refused"),
]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--playbox", default=os.environ.get("PLAYBOX_BIN") or str(ROOT / "target" / "debug" / "playbox"))
    ap.add_argument("--replay", action="store_true", help="also re-train the promotions here on seeds revealed after the round (about 3 minutes on CPU)")
    ap.add_argument("--keep", help="keep everything under this directory instead of a temporary one")
    a = ap.parse_args()
    pb = a.playbox
    if not Path(pb).exists():
        die(f"{pb} does not exist (cargo build -p playbox-cli, or --playbox PATH)")
    if run("docker", "info", check=False).returncode != 0:
        die("Docker is not running (the guests' tasks run in a container)")
    for mod in ("cryptography", "numpy", "torch"):
        if run(sys.executable, "-c", f"import {mod}", check=False).returncode != 0:
            die(f"python3 needs {mod}")

    work = Path(a.keep or tempfile.mkdtemp(prefix="playbox-live-")).resolve()
    work.mkdir(parents=True, exist_ok=True)
    procs: list[subprocess.Popen] = []
    guests: list[Guest] = []
    try:
        go(pb, work, a.replay, procs, guests)
    finally:
        for g in guests:
            g.close()
        for p in procs:
            if p.poll() is None:
                p.terminate()
        if not a.keep:
            shutil.rmtree(work, ignore_errors=True)


def go(pb: str, work: Path, replay: bool, procs: list, guests: list) -> None:
    say("1. A host opens a room on a repository whose candidate.py is the F3 baseline")
    repo = work / "repo"
    run("git", "init", "-q", "-b", "main", repo)
    shutil.copy(F3 / "baseline.py", repo / "candidate.py")
    (repo / "harness.py").write_text("# stands in for files the gate must never take from a member\n")
    run("git", "add", ".", cwd=repo)
    run(*GIT, "commit", "-qm", "the starting point: the baseline", cwd=repo)
    host = subprocess.Popen([pb, "up", "--local", "--home", work / "host", "--state", work / "state", "--repo", repo, "--docker", "--console"],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    procs.append(host)
    host_lines: list[str] = []
    threading.Thread(target=lambda: [host_lines.append(l.strip()) for l in host.stdout], daemon=True).start()

    def new_link() -> str:
        seen = sum(l.startswith("playbox://") for l in host_lines)
        host.stdin.write("link\n")
        host.stdin.flush()
        for _ in range(120):
            if sum(l.startswith("playbox://") for l in host_lines) > seen:
                return [l for l in host_lines if l.startswith("playbox://")][-1]
            time.sleep(0.25)
        die("the host gave no link:\n" + "\n".join(host_lines[-10:]))

    for _ in range(240):
        if any("room is up" in l for l in host_lines):
            break
        time.sleep(0.25)
    else:
        die("the host did not start:\n" + "\n".join(host_lines[-10:]))
    ok("the room is up; each guest below gets its own single-use link")

    say("2. The host runs the gate beside the room: it calibrates on the baseline, commits to a sealed holdout, and writes the genesis receipt")
    gate_work = work / "gate"
    gate = subprocess.Popen([sys.executable, "-u", str(F3 / "room_gate.py"), "--work", str(gate_work), "--repo", str(repo), "--playbox", pb, "--stop-file", str(work / "stop"), "--reveal"],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env={**os.environ, "F3_SUBSTRATE": "synthetic"})
    procs.append(gate)
    gate_lines: list[str] = []

    def pump():
        for l in gate.stdout:
            gate_lines.append(l.rstrip())
            print("   | " + l.rstrip()[:150], flush=True)

    threading.Thread(target=pump, daemon=True).start()

    def wait_gate(pred, what: str, timeout: float = 900) -> None:
        t0 = time.time()
        while time.time() - t0 < timeout:
            if any(pred(l) for l in gate_lines):
                return
            if gate.poll() is not None and not any(pred(l) for l in gate_lines):
                die(f"the gate stopped before {what}:\n" + "\n".join(gate_lines[-8:]))
            time.sleep(0.5)
        die(f"timed out waiting for {what}")

    wait_gate(lambda l: l == "GATE READY", "calibration")
    pub = (gate_work / "host.pub").read_text().strip()
    ok(f"gate key {pub[:16]}…; main's candidate.py is the frontier")

    say("3. Guests take turns: pull the frontier, run a proposal as a sandboxed task, push; the gate answers with a signed receipt")
    results = []
    for i, (label, command, expect) in enumerate(PROPOSALS):
        home, ws = work / f"guest{i}", work / f"ws{i}"
        g = Guest(pb, home, ws)
        guests.append(g)
        g.call("join", link=new_link(), transport="local", workspace=str(ws))
        g.call("pull", ref="main", checkout=True)
        frontier_sha = run("git", "rev-parse", "--short", "HEAD", cwd=ws).stdout.strip()
        print(f"\n   guest {i + 1} ({label}) pulled main at {frontier_sha} and runs its proposal in the sandbox", flush=True)
        g.call("delegate", base="main", command=command, apply=True)
        run("git", "checkout", "-q", "-b", f"try-{label}", cwd=ws)
        run("git", "add", "-A", cwd=ws)
        if run("git", "diff", "--cached", "--quiet", cwd=ws, check=False).returncode == 0:
            die(f"the sandboxed task for {label} returned no patch")
        run(*GIT, "commit", "-qm", label, cwd=ws)
        before = len([l for l in gate_lines if " PROMOTE " in l or " REJECT " in l])
        g.call("push", branch=f"try-{label}")
        if expect == "refused":
            wait_gate(lambda l: l.startswith("refused"), "the refusal", 120)
        else:
            wait_gate(lambda l: len([x for x in gate_lines if " PROMOTE " in x or " REJECT " in x]) > before, "the receipt")
        line = next(l for l in reversed(gate_lines) if (" PROMOTE " in l or " REJECT " in l or l.startswith("refused")))
        results.append((label, expect, line))
        g.close()
        guests.remove(g)

    say("4. What each push got")
    mismatch = []
    for label, expect, line in results:
        got = "refused" if line.startswith("refused") else ("promote" if " PROMOTE " in line else line.split()[3])
        if expect not in line and got != expect:
            mismatch.append((label, expect, got))
        print(f"   {label:24s} {line.strip()[:110]}")
    if mismatch:
        die(f"unexpected outcomes: {mismatch}")
    main_head = run("git", "rev-parse", "--short", "main", cwd=repo).stdout.strip()
    promoted = sum(e == "promote" for _, e, _ in results)
    count = int(run("git", "rev-list", "--count", "main", cwd=repo).stdout)
    if count != 1 + promoted:
        die(f"main has {count} commits, expected {1 + promoted}: only promotions may move it")
    if "gate is mine" in (repo / "harness.py").read_text():
        die("the host's harness.py was changed by a push")
    ok(f"main moved {promoted} times, to {main_head}, and only for the promotions; the rejected cheat and the out-of-bounds push never reached it")

    (work / "stop").write_text("done\n")
    say("5. The gate opens the holdout; the receipts are verified from the chain, the archived candidates and this repository alone")
    wait_gate(lambda l: l.startswith("reveal receipt"), "the reveal", 120)
    gate.wait(timeout=30)
    verify = run(sys.executable, F3 / "verify.py", "--work", gate_work, "--host-pubkey", pub, check=False)
    for l in verify.stdout.splitlines():
        if l.startswith("[FAIL]") or l.startswith("RESULT"):
            print("   | " + l[:150])
    if "RESULT: VERIFIED" not in verify.stdout:
        die("the live run did not verify:\n" + verify.stdout[-1500:])
    ok(f"{verify.stdout.count('[PASS]')} checks pass; every promotion above is re-derived from signed measurements")
    if '"caught_false_promotions": 1' not in verify.stdout:
        die("the injected cheat is not recorded as caught")
    ok("the proposal that trained on the visible dev set beat the threshold there and was rejected on the holdout it never saw")

    say("6. The host cannot quietly promote the cheat either")
    forged = work / "forged"
    shutil.copytree(gate_work, forged, ignore=shutil.ignore_patterns("custodian", "host.key"))
    rec = forged / "receipts.jsonl"
    lines = rec.read_text().splitlines()
    for i, l in enumerate(lines):
        if '"holdout_failed"' in l:
            lines[i] = l.replace('"reject"', '"promote"', 1)
            break
    else:
        die("no holdout_failed receipt to forge")
    rec.write_text("\n".join(lines) + "\n")
    bad = run(sys.executable, F3 / "verify.py", "--work", forged, "--host-pubkey", pub, check=False)
    if "RESULT: VERIFIED" in bad.stdout:
        die("a forged promotion verified")
    ok("rejected: " + next((l[7:127] for l in bad.stdout.splitlines() if l.startswith("[FAIL]")), "does not verify"))

    if replay:
        say("7. Replay: train each promotion again on seeds the proposals never saw")
        r = run(sys.executable, F3 / "verify.py", "--work", gate_work, "--host-pubkey", pub, "--replay", "all", check=False)
        for l in r.stdout.splitlines():
            if "replay" in l or l.startswith("RESULT"):
                print("   | " + l[:170])
        if "RESULT: VERIFIED" not in r.stdout:
            die("the replay did not verify")
        ok("every promotion reproduces here")

    say("Done. One run, end to end: a room, sandboxed tasks, pushes, a gate that signed what it decided, and a verifier that trusts none of it.")


if __name__ == "__main__":
    main()
