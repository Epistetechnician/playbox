"""Playbox orchestration: LangGraph delegation graphs (M7), steering chains (E1) and the steering search (E4).

The LangGraph pieces load on first use, so the stdlib-only modules (`mcp`, `steer_search`) work in an environment
without it.
"""
from .a2a import Peer, PeerError, TaskResult

__all__ = ["Peer", "PeerError", "TaskResult", "Subtask", "build_graph", "delegate_and_merge"]


def __getattr__(name):
    if name in ("Subtask", "build_graph", "delegate_and_merge"):
        from . import graph

        return getattr(graph, name)
    raise AttributeError(name)
