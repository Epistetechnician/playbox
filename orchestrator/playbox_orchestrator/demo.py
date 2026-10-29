"""Three real hosts, three bridges, one orchestrated job.

    uv run python -m playbox_orchestrator.demo [--runtime docker] [--conflict]

Starts three `playbox up` hosts (each with its own copy of a small repo and a real sandbox runtime),
joins a `playbox bridge` to each, fans three subtasks out in parallel, and merges the patches.
With --conflict a fourth subtask edits the same line as the first and must be reported, not applied.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from .a2a import Peer
from .graph import Subtask, delegate_and_merge

ROOT = Path(__file__).resolve().parents[2]
BIN = os.environ.get("PLAYBOX_BIN", str(ROOT / "target/debug/playbox"))


def make_repo(path: Path) -> None:
    subprocess.run(["git", "init", "-q", "-b", "main", str(path)], check=True)
    for k, v in (("user.email", "d@e"), ("user.name", "demo")):
        subprocess.run(["git", "-C", str(path), "config", k, v], check=True)
    (path / "a.txt").write_text("one\ntwo\nthree\n")
    subprocess.run(["git", "-C", str(path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(path), "commit", "-q", "-m", "init"], check=True)


def wait_for(proc: subprocess.Popen, log: Path, pattern: str, secs: float = 90) -> str:
    end = time.time() + secs
    while time.time() < end:
        m = re.search(pattern, log.read_text(), re.M)
        if m:
            return m.group(0)
        if proc.poll() is not None:
            raise RuntimeError(f"process exited early:\n{log.read_text()}")
        time.sleep(0.3)
    raise TimeoutError(f"no {pattern!r} in {log}:\n{log.read_text()}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--runtime", default="docker")
    ap.add_argument("--conflict", action="store_true")
    args = ap.parse_args()
    procs: list[subprocess.Popen] = []
    with tempfile.TemporaryDirectory(prefix="playbox-demo-") as tmp:
        tmp = Path(tmp)
        try:
            peers = []
            for i in range(3):
                repo = tmp / f"repo{i}"
                make_repo(repo)
                hl = tmp / f"host{i}.log"
                host = subprocess.Popen([BIN, "up", "--local", "--repo", str(repo), "--runtime", args.runtime], stdout=hl.open("w"), stderr=subprocess.STDOUT, text=True)
                procs.append(host)
                link = wait_for(host, hl, r"playbox://\S+")
                port = 18787 + i
                bl = tmp / f"bridge{i}.log"
                bridge = subprocess.Popen([BIN, "bridge", link, "--local", "--listen", f"127.0.0.1:{port}"], stdout=bl.open("w"), stderr=subprocess.STDOUT, text=True)
                procs.append(bridge)
                wait_for(bridge, bl, r"bridge up")
                peers.append(Peer(f"http://127.0.0.1:{port}", timeout=240))
                print(f"peer {i}: host+bridge up on :{port} ({args.runtime})")
            subs = [
                Subtask("edit-line-1", peers[0], "sed -i 's/^one$/ONE/' a.txt"),
                Subtask("add-file", peers[1], "echo from peer 1 > b.txt"),
                Subtask("edit-line-3", peers[2], "sed -i 's/^three$/THREE/' a.txt"),
            ]
            if args.conflict:
                subs.append(Subtask("conflicting-edit", peers[1], "sed -i 's/^one$/UNO/' a.txt"))
            t0 = time.time()
            report = delegate_and_merge(str(tmp / "repo0"), subs)
            print(f"merged in {time.time() - t0:.1f}s")
            print(json.dumps({k: v for k, v in report.items() if k != "files"}, indent=2))
            print("merged files:", json.dumps(report["files"], indent=2))
            ok = report["applied"] == [s.name for s in subs if s.name != "conflicting-edit"] and not report["failed"]
            if args.conflict:
                ok = ok and [c["subtask"] for c in report["conflicts"]] == ["conflicting-edit"]
            print("RESULT:", "PASS" if ok else "FAIL")
            return 0 if ok else 1
        finally:
            for p in procs:
                p.terminate()


if __name__ == "__main__":
    sys.exit(main())
