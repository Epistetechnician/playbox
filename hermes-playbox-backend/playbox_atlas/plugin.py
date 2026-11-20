"""Hermes plugin exposing `atlas_query`: zero-shell-roundtrip hierarchical KG2KV + HiKVP repo/memory lookup."""
from __future__ import annotations

import json
import os
from pathlib import Path

from .hikvp import HiKVPIndex, format_brief
from .kg2kv import build_repo_triples, load_room_memory_jsonl

_CACHED_INDEX: tuple[str, HiKVPIndex] | None = None


def _load_index() -> HiKVPIndex | None:
    global _CACHED_INDEX
    idx_path = os.environ.get("PLAYBOX_ATLAS_INDEX", "").strip()
    repo_path = os.environ.get("PLAYBOX_ATLAS_REPO", "").strip()
    mem_path = os.environ.get("PLAYBOX_ATLAS_MEMORY", "").strip()
    key = f"{idx_path}|{repo_path}|{mem_path}"
    if _CACHED_INDEX is not None and _CACHED_INDEX[0] == key:
        return _CACHED_INDEX[1]
    if idx_path and Path(idx_path).exists():
        idx = HiKVPIndex.load(idx_path)
    elif repo_path and Path(repo_path).is_dir():
        triples = build_repo_triples(repo_path)
        if mem_path:
            triples.extend(load_room_memory_jsonl(mem_path))
        idx = HiKVPIndex.build(triples)
    else:
        return None
    _CACHED_INDEX = (key, idx)
    return idx


def query_atlas(query: str, top_k: int = 12, expand_hops: int = 3) -> str:
    """Query the verified Playbox AtlasKV index (quarantined peer entries excluded)."""
    idx = _load_index()
    if idx is None:
        return "[playbox-atlas: no index configured (set PLAYBOX_ATLAS_INDEX or PLAYBOX_ATLAS_REPO)]"
    hits = idx.query(
        query,
        top_leaf=max(1, min(int(top_k), 32)),
        expand_hops=max(0, min(int(expand_hops), 3)),
        allow_untrusted=False,
    )
    if not hits:
        return "[playbox-atlas: no matching verified symbols or lessons]"
    return format_brief(hits, max_chars=3200)


def register(ctx):
    if not hasattr(ctx, "register_tool"):
        return
    schema = {
        "name": "atlas_query",
        "description": (
            "Hierarchical KG2KV + HiKVP lookup over the verified repository call graph and accepted room lessons. "
            "Use before running broad grep/find shell commands to locate exact files, classes, methods, and callers."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Bug symptom, symbol, or module question"},
                "top_k": {"type": "integer", "description": "Maximum triples to return (1-32, default 12)"},
            },
            "required": ["query"],
        },
    }

    def _handler(args: dict, **_) -> str:
        return json.dumps({"brief": query_atlas(args.get("query", ""), args.get("top_k", 12))})

    ctx.register_tool(
        name="atlas_query",
        toolset="file",
        schema=schema,
        handler=_handler,
        description=schema["description"],
    )
