"""Reliability and statistics for the two-agent telepathy trials (tools/telepathy_hermes.py).

Failures are split in two. An infrastructure failure (agent timed out, daemon down, no vector written, B gave no parsable
answer, machine out of memory) says nothing about the channel: the trial is retried from a clean directory and, if it never
succeeds, reported as infra and left out of accuracy. A model failure (B gave a valid answer that is wrong) is data and is
never retried. Retrying only on infrastructure failure keeps the retries from inflating accuracy.
"""
from __future__ import annotations

import json
import math
import os
import signal
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path


class InfraError(RuntimeError):
    """The trial could not be run; says nothing about whether the thought got through."""


def free_gb() -> float:
    """Available memory in GB: macOS vm_stat, else /proc/meminfo, else unknown (treated as plenty)."""
    try:
        out = subprocess.run(["vm_stat"], capture_output=True, text=True, timeout=10).stdout
        page = int(out.split("page size of ")[1].split()[0])
        return sum(int(l.split(":")[1].strip(" .")) for l in out.splitlines()
                   if l.split(":")[0] in ("Pages free", "Pages inactive", "Pages speculative")) * page / 2**30
    except (OSError, IndexError, ValueError, subprocess.SubprocessError):
        pass
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) / 2**20
    except OSError:
        pass
    return float("inf")


def wait_for_memory(min_gb: float, patience_s: int = 600, poll_s: int = 10) -> None:
    deadline = time.time() + patience_s
    while free_gb() < min_gb:
        if time.time() > deadline:
            raise InfraError(f"less than {min_gb} GB free for {patience_s} s")
        time.sleep(poll_s)


def run_agent(argv: list[str], cwd: Path | None, timeout: int) -> tuple[int, str, str]:
    """Run one agent process in its own process group; on timeout kill the whole group, so no orphaned child keeps
    a request open against the daemon and queues behind later trials."""
    p = subprocess.Popen(argv, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
    try:
        out, err = p.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        p.communicate()
        raise InfraError(f"agent timed out after {timeout} s")
    return p.returncode, out, err


# ---- daemon -------------------------------------------------------------------------------------------------------

def daemon_up(url: str, timeout: float = 3) -> bool:
    try:
        return urllib.request.urlopen(url.rstrip("/") + "/health", timeout=timeout).read().startswith(b"ok")
    except OSError:
        return False


_START = threading.Lock()  # two workers must not both launch the daemon


def ensure_daemon(url: str, py: str | Path, root: Path, port: int, wait_s: int = 300) -> None:
    """Start the resident daemon if it is not answering, and wait for it. A daemon that died mid-run is the usual
    cause of an agent saying 'sent' with no vector written."""
    with _START:
        _ensure(url, py, root, port, wait_s)


def _ensure(url: str, py: str | Path, root: Path, port: int, wait_s: int) -> None:
    if daemon_up(url):
        return
    log = open(root / "results/telepathy/daemon.log", "ab")
    subprocess.Popen([str(py), "-m", "playbox_steer.telepathy_agent", "serve", "--port", str(port)], cwd=root,
                     env={**os.environ, "PYTHONPATH": str(root)}, stdout=log, stderr=log, start_new_session=True)
    deadline = time.time() + wait_s
    while time.time() < deadline:
        if daemon_up(url):
            return
        time.sleep(2)
    raise InfraError(f"telepathy daemon did not come up at {url} within {wait_s} s")


# ---- answers ------------------------------------------------------------------------------------------------------

def parse_answer(text: str, choices: list[str]) -> str | None:
    """The last 'ANSWER:' line, normalised; None if absent or not one of the offered topics (or 'none')."""
    lines = [l for l in text.splitlines() if l.strip().lstrip("*`> ").upper().startswith("ANSWER:")]
    if not lines:
        return None
    g = lines[-1].split(":", 1)[1].strip().lower().strip(".*`\"' ")
    return g if g in choices or g == "none" else None


def vector_ok(path: Path) -> bool:
    return path.is_file() and path.stat().st_size > 256


# ---- statistics ---------------------------------------------------------------------------------------------------

def wilson(k: int, n: int, z: float = 1.96) -> tuple[float, float]:
    if n == 0:
        return (0.0, 1.0)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, c - h), min(1.0, c + h))


def binom_sf(k: int, n: int, p: float) -> float:
    """P(X >= k) for X ~ Binomial(n, p): the one-sided exact p-value against chance."""
    return min(1.0, sum(math.comb(n, i) * p**i * (1 - p) ** (n - i) for i in range(k, n + 1)))


def summarize(results: list[dict], n_topics: int) -> dict:
    """Accuracy over trials that ran (status ok), split into concept trials and no-concept controls, per direction.
    Chance for a concept trial is 1/(n_topics+1): B may also answer 'none'."""
    ran = [r for r in results if "error" not in r]
    infra = [r for r in results if "error" in r]
    chance = 1 / (n_topics + 1)

    def block(rs: list[dict]) -> dict:
        n, k = len(rs), sum(bool(r["ok"]) for r in rs)
        lo, hi = wilson(k, n)
        return {"n": n, "correct": k, "acc": round(k / n, 4) if n else None, "ci95": [round(lo, 4), round(hi, 4)],
                "p_vs_chance": binom_sf(k, n, chance) if n else None}

    concept = [r for r in ran if r["truth"] != "none"]
    ctrl = [r for r in ran if r["truth"] == "none"]
    dirs = sorted({f"{r['src']}->{r['dst']}" for r in ran})
    return {"chance": round(chance, 4), "concept": block(concept), "control": block(ctrl),
            "false_positive_rate": round(sum(r["guess"] != "none" for r in ctrl) / len(ctrl), 4) if ctrl else None,
            "by_direction": {d: block([r for r in concept if f"{r['src']}->{r['dst']}" == d]) for d in dirs},
            "infra_failures": len(infra), "infra_reasons": sorted({r["error"][:80] for r in infra}),
            "mean_attempts": round(sum(r.get("attempts", 1) for r in ran) / len(ran), 2) if ran else None}


def load_results(path: Path) -> list[dict]:
    return json.loads(path.read_text()) if path.exists() else []


def merge_results(old: list[dict], new: list[dict]) -> list[dict]:
    """Later results replace earlier ones for the same trial index; a clean result is never replaced by an infra error."""
    by = {r["i"]: r for r in old}
    for r in new:
        if r["i"] in by and "error" not in by[r["i"]] and "error" in r:
            continue
        by[r["i"]] = r
    return [by[i] for i in sorted(by)]


if __name__ == "__main__":  # python -m playbox_steer.telepathy_harness results.json N_TOPICS
    print(json.dumps(summarize(json.loads(Path(sys.argv[1]).read_text()), int(sys.argv[2])), indent=1))
