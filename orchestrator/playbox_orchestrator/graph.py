"""Fan a job out to several peers in parallel, then merge what they send back.

    plan ──► (one `delegate` per subtask, in parallel) ──► merge

The merge applies each peer's patch to a scratch clone of the base, in subtask order, with a 3-way
apply. A patch that no longer applies (it conflicts with an earlier one) is left out and reported,
never forced. Patches come from other members, so they are applied with hooks off and only inside
the scratch clone; nothing is pushed anywhere.
"""
from __future__ import annotations

import operator
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Annotated, TypedDict

from langgraph.graph import END, START, StateGraph
from langgraph.types import Send

from .a2a import Peer, PeerError, TaskResult


@dataclass(frozen=True)
class Subtask:
    name: str
    peer: Peer
    cmd: str
    base: str = "main"


class State(TypedDict, total=False):
    subtasks: list[Subtask]
    repo: str
    results: Annotated[list[tuple[int, str, TaskResult | str]], operator.add]
    merged: dict


class _Job(TypedDict):
    index: int
    subtask: Subtask


def _git(repo: Path, *args: str, stdin: str | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", *args],
        input=stdin, capture_output=True, text=True,
    )


def _plan(state: State):
    return [Send("delegate", _Job(index=i, subtask=s)) for i, s in enumerate(state["subtasks"])]


def _delegate(job: _Job):
    s = job["subtask"]
    try:
        r: TaskResult | str = s.peer.run(s.cmd, base=s.base)
    except PeerError as e:
        r = str(e)
    return {"results": [(job["index"], s.name, r)]}


def _merge(state: State):
    ordered = sorted(state.get("results", []), key=lambda t: t[0])
    report = {"applied": [], "conflicts": [], "failed": [], "empty": []}
    with tempfile.TemporaryDirectory(prefix="playbox-merge-") as tmp:
        work = Path(tmp) / "work"
        cp = subprocess.run(["git", "clone", "-q", "--no-hardlinks", state["repo"], str(work)], capture_output=True, text=True)
        if cp.returncode:
            raise RuntimeError(f"cannot clone {state['repo']}: {cp.stderr.strip()}")
        _git(work, "config", "user.email", "orchestrator@playbox.local")
        _git(work, "config", "user.name", "playbox-orchestrator")
        for _, name, r in ordered:
            if isinstance(r, str):
                report["failed"].append({"subtask": name, "reason": r})
            elif not r.ok:
                report["failed"].append({"subtask": name, "reason": f"{r.state}: {r.stderr.strip()[:200]}"})
            elif not r.patch.strip():
                report["empty"].append(name)
            else:
                p = _git(work, "apply", "--3way", "--whitespace=nowarn", "-", stdin=r.patch)
                if p.returncode:
                    _git(work, "reset", "-q", "--hard")
                    report["conflicts"].append({"subtask": name, "reason": p.stderr.strip()[:300]})
                else:
                    _git(work, "add", "-A")
                    _git(work, "commit", "-q", "-m", f"{name} (via {r.peer})")
                    report["applied"].append(name)
        report["head"] = _git(work, "rev-parse", "HEAD").stdout.strip()
        report["diff"] = _git(work, "diff", "--stat", "HEAD~%d" % len(report["applied"])).stdout if report["applied"] else ""
        report["files"] = {p.relative_to(work).as_posix(): p.read_text(errors="replace")
                           for p in sorted(work.rglob("*")) if p.is_file() and ".git" not in p.relative_to(work).parts}
    return {"merged": report}


def build_graph():
    g = StateGraph(State)
    g.add_node("delegate", _delegate)
    g.add_node("merge", _merge)
    g.add_conditional_edges(START, _plan, ["delegate"])
    g.add_edge("delegate", "merge")
    g.add_edge("merge", END)
    return g.compile()


def delegate_and_merge(repo: str, subtasks: list[Subtask]) -> dict:
    """Run every subtask on its peer, merge the patches onto a clone of `repo`, return the report."""
    return build_graph().invoke({"repo": repo, "subtasks": subtasks})["merged"]
