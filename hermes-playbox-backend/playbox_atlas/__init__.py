"""AtlasKV-inspired hierarchical KG2KV + HiKVP repo & room memory index with Playbox provenance.

Adapts the two core primitives of HKUST-KnowComp/AtlasKV (ICLR 2026, MIT) to model-agnostic
agent harnesses (working with Claude, Gemini, GPT, and open-weights models alike):
1. `KG2KV`: converts source code structure (AST modules, classes, methods, functions, imports,
   call-graph edges, docstrings) and verified Playbox room memory (`cue / lesson / evidence`) into
   compact `(entity, relation, value)` triples tagged with cryptographic provenance & trust tier.
2. `HiKVP`: organizes keys into a 3-level hierarchy (`Root [package/domain] -> Inter [file/class]
   -> Leaf [symbol/fact triple]`) and performs sub-linear top-k hierarchical beam pruning with
   optional 1-hop call-graph expansion and strict Playbox quarantine enforcement.
"""
from .aiproxy_embed import AiproxyEmbedder, AiproxyReranker
from .hikvp import HiKVPIndex, QueryHit, format_brief
from .kg2kv import Triple, append_verified_lesson, build_repo_triples, load_room_memory_jsonl, load_room_memory_triples
from .plugin import query_atlas, register

__all__ = [
    "AiproxyEmbedder",
    "AiproxyReranker",
    "HiKVPIndex",
    "QueryHit",
    "Triple",
    "append_verified_lesson",
    "build_repo_triples",
    "format_brief",
    "load_room_memory_jsonl",
    "load_room_memory_triples",
    "query_atlas",
    "register",
]
