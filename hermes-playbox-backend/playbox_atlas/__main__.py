"""CLI entrypoint for building and querying a Playbox AtlasKV (KG2KV + HiKVP) index."""
from __future__ import annotations

import argparse
import json
from pathlib import Path

from .hikvp import HiKVPIndex, format_brief
from .kg2kv import build_repo_triples, load_room_memory_jsonl


def main() -> None:
    p = argparse.ArgumentParser(prog="playbox_atlas", description="AtlasKV-inspired hierarchical repo & room memory index")
    sub = p.add_subparsers(dest="cmd", required=True)

    b = sub.add_parser("build", help="Index a git repo (and optional room memory JSONL)")
    b.add_argument("repo", type=Path)
    b.add_argument("-o", "--out", type=Path, required=True)
    b.add_argument("--memory", type=Path, default=None)
    b.add_argument("--include-tests", action="store_true")

    q = sub.add_parser("query", help="Query a saved HiKVP index")
    q.add_argument("index", type=Path)
    q.add_argument("text")
    q.add_argument("--top", type=int, default=12)
    q.add_argument("--json", action="store_true", dest="as_json")

    args = p.parse_args()
    if args.cmd == "build":
        triples = build_repo_triples(args.repo, include_tests=args.include_tests)
        if args.memory:
            triples.extend(load_room_memory_jsonl(args.memory))
        idx = HiKVPIndex.build(triples)
        idx.save(args.out)
        print(json.dumps({"saved": str(args.out), **idx.stats()}))
    else:
        idx = HiKVPIndex.load(args.index)
        hits = idx.query(args.text, top_leaf=args.top)
        if args.as_json:
            print(json.dumps([{"score": h.score, "hop": h.hop, **h.triple.to_dict()} for h in hits], indent=2))
        else:
            print(format_brief(hits))


if __name__ == "__main__":
    main()
