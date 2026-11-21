"""Hermes plugin exposing `atlas_query` and `atlas_parametric_qa`: verified HiKVP + zero-token Parametric KV lookup."""
from __future__ import annotations

import json
import os
from pathlib import Path

from .hikvp import HiKVPIndex, format_brief
from .kg2kv import build_repo_triples, load_room_memory_jsonl

_CACHED_INDEX: tuple[str, HiKVPIndex] | None = None
_CACHED_PARAMETRIC: tuple[str, object, list, dict] | None = None


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


def query_parametric_atlas(query: str, top_clusters: int = 2) -> dict:
    """Answer `query` with zero KB prompt tokens by mounting a PQ-verified `.pt` shard (`PLAYBOX_ATLAS_SHARD`)."""
    global _CACHED_PARAMETRIC
    shard_dir = os.environ.get("PLAYBOX_ATLAS_SHARD", "").strip()
    if not shard_dir or not (Path(shard_dir) / "manifest.json").is_file():
        return {"ok": False, "error": "no signed shard configured (set PLAYBOX_ATLAS_SHARD)"}
    idx = _load_index()
    if idx is None:
        return {"ok": False, "error": "no HiKVP index configured (set PLAYBOX_ATLAS_INDEX or PLAYBOX_ATLAS_REPO)"}

    expected_pk = os.environ.get("PLAYBOX_ATLAS_PUBKEY", "").strip() or None
    expected_pq_pk = os.environ.get("PLAYBOX_ATLAS_PQ_PUBKEY", "").strip() or None
    require_pq = os.environ.get("PLAYBOX_ATLAS_REQUIRE_PQ", "0").strip() == "1"
    key = f"{shard_dir}|{expected_pk}|{expected_pq_pk}|{require_pq}"

    from .parametric import ParametricAtlasKVEngine, load_verified_kb_shard, select_hikvp_clusters

    if _CACHED_PARAMETRIC is None or _CACHED_PARAMETRIC[0] != key:
        clusters, meta = load_verified_kb_shard(
            shard_dir,
            expected_pubkey_hex=expected_pk,
            expected_pq_pubkey_hex=expected_pq_pk,
            require_pq=require_pq,
        )
        engine = ParametricAtlasKVEngine(model_id=meta["model_id"])
        _CACHED_PARAMETRIC = (key, engine, clusters, meta)
    else:
        _, engine, clusters, meta = _CACHED_PARAMETRIC

    sel = select_hikvp_clusters(idx, clusters, query, top_clusters=max(1, min(int(top_clusters), 4)))
    q_prompt = f"Question: {query.strip()}\nAnswer:"
    ans, metrics = engine.generate_with_injected_kv(sel, q_prompt, max_new_tokens=32)
    return {
        "ok": True,
        "answer": ans,
        "sig_alg": meta.get("sig_alg", "Ed25519"),
        "pq_verified": bool(meta.get("pq_verified", False)),
        "clusters_mounted": [c.inter_key for c in sel if c.inter_key != "__preamble__"],
        **metrics,
    }


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

    if os.environ.get("PLAYBOX_ATLAS_SHARD", "").strip():
        p_schema = {
            "name": "atlas_parametric_qa",
            "description": (
                "Zero-prompt-token local Parametric AtlasKV lookup over a PQ-signed `.pt` KV shard. "
                "Answers exact symbol/file/line questions locally before spending remote API tokens."
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Exact symbol, file path, or line number question"},
                    "top_clusters": {"type": "integer", "description": "Clusters to mount (1-4, default 2)"},
                },
                "required": ["query"],
            },
        }

        def _p_handler(args: dict, **_) -> str:
            return json.dumps(query_parametric_atlas(args.get("query", ""), args.get("top_clusters", 2)))

        ctx.register_tool(
            name="atlas_parametric_qa",
            toolset="file",
            schema=p_schema,
            handler=_p_handler,
            description=p_schema["description"],
        )
