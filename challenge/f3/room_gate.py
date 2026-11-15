"""The host's gate for a live room: what members push is scored by the F3 harness, and only a signed promotion moves `main`.

    room_gate.py --work DIR --repo REPO --playbox PLAYBOX [--stop-file FILE] [--reveal]

Run by the host's owner next to `playbox up --repo REPO`. It never runs inside the sandbox: it holds the custodian's holdout.
Each pending push under refs/playbox/members/ is handled once:

  1. refused without a receipt if it is not built on the tip of main, or touches anything but `candidate.py`
     (a member can change the candidate, not the harness, the receipts or this repository's other files);
  2. otherwise `harness.submit` scores it against the frontier on the training seeds, checks the sealed holdout and signs a
     decision receipt (promote or reject, with the reason);
  3. a promotion is fast-forwarded onto main with `playbox review ... accept`; anything else is retired with `reject`.

main's `candidate.py` is therefore always the frontier, and a member who pulls main starts from it. The gate
runs until it is killed, or until --stop-file exists and nothing is pending; with --reveal it then opens the holdout (the last
receipt), so anyone can re-derive the chain.
The harness is imported unchanged, so a run through this gate verifies with challenge/f3/verify.py like the recorded one.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import harness  # noqa: E402

CANDIDATE = "candidate.py"
PENDING = "refs/playbox/members/"
MAX_BYTES = 200_000


def git(repo: Path, *a: str, check: bool = True) -> str:
    r = subprocess.run(["git", *a], cwd=repo, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(a)}: {r.stderr.strip()}")
    return r.stdout.strip()


def blob(repo: Path, spec: str) -> bytes:
    return subprocess.run(["git", "show", spec], cwd=repo, capture_output=True, check=True).stdout


def pending(repo: Path) -> list[tuple[str, str]]:
    out = git(repo, "for-each-ref", PENDING, "--format=%(refname) %(objectname)")
    return [tuple(line.split(" ", 1)) for line in out.splitlines() if line]


def screen(repo: Path, ref: str) -> str | None:
    """Why this push cannot be scored at all, or None. These are refusals, not verdicts: no receipt."""
    if subprocess.run(["git", "merge-base", "--is-ancestor", "refs/heads/main", ref], cwd=repo).returncode != 0:
        return "not built on the tip of main (pull the frontier and rebase)"
    changed = git(repo, "diff", "--name-only", "refs/heads/main", ref).splitlines()
    if changed != [CANDIDATE]:
        return f"changes {changed or 'nothing'}; a push may change only {CANDIDATE}"
    size = int(git(repo, "cat-file", "-s", f"{ref}:{CANDIDATE}"))
    if size > MAX_BYTES:
        return f"{CANDIDATE} is {size} bytes (limit {MAX_BYTES})"
    return None


def review(playbox: str, repo: Path, verb: str, ref: str) -> None:
    r = subprocess.run([playbox, "review", str(repo), verb, ref], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"playbox review {verb} {ref}: {(r.stdout + r.stderr).strip()}")


def handle(work: Path, repo: Path, playbox: str, ref: str, head: str, log) -> dict | None:
    member, branch = ref[len(PENDING):].split("/", 1)
    author = f"{member[:8]}/{branch}"
    why = screen(repo, ref)
    if why:
        log(f"refused {author}: {why}")
        review(playbox, repo, "reject", ref)
        return None
    with tempfile.TemporaryDirectory() as tmp:
        cand = Path(tmp) / "candidate.py"
        cand.write_bytes(blob(repo, f"{ref}:{CANDIDATE}"))
        body = harness.submit(work, cand, author, log=log)
    if body["verdict"] == "promote":
        review(playbox, repo, "accept", ref)
        front = harness._state(work)["frontier"][-1]["sha256"]
        tip = harness.sha256_bytes(blob(repo, f"refs/heads/main:{CANDIDATE}"))
        if tip != front:
            raise RuntimeError("main's candidate.py is not the harness frontier; stopping rather than drifting")
    else:
        review(playbox, repo, "reject", ref)
    return body


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--playbox", required=True)
    ap.add_argument("--stop-file", help="finish once this file exists and no push is pending")
    ap.add_argument("--reveal", action="store_true", help="open the holdout when done")
    ap.add_argument("--poll", type=float, default=0.5)
    a = ap.parse_args()
    work, repo = Path(a.work).resolve(), Path(a.repo).resolve()
    log = lambda s: print(s, flush=True)  # noqa: E731
    if not (work / "state.json").exists():
        harness.init(work, log=log)
    # The room's first candidate.py is the harness baseline, byte for byte, so main starts at the frontier.
    base = harness.sha256_bytes(blob(repo, f"refs/heads/main:{CANDIDATE}"))
    if base != harness._state(work)["frontier"][-1]["sha256"]:
        log("the repository's candidate.py is not the harness baseline; refusing to start")
        return 2
    print("GATE READY", flush=True)
    while True:
        queue = pending(repo)
        for ref, head in queue:
            handle(work, repo, a.playbox, ref, head, log)
        if not queue and a.stop_file and Path(a.stop_file).exists():
            break
        time.sleep(a.poll)
    if a.reveal:
        harness.reveal(work, log=log)
    return 0


if __name__ == "__main__":
    sys.exit(main())
