#!/usr/bin/env python3
"""CLI runner for the 3-arm benchmark (S-RAG vs S-CTX vs S-ROUTED) on the 128K CRM corpus.

Persists every cell JSON immediately to --out-dir (e.g. Google Drive) and writes
turns.jsonl, summary.json, and mcnemar.json at completion.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import sys
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Tuple

STEER_ROOT = Path(__file__).resolve().parents[1]
if str(STEER_ROOT) not in sys.path:
    sys.path.insert(0, str(STEER_ROOT))

from playbox_steer import kv_routed as KR  # noqa: E402


def _load_bundle_scorer(bundle_dir: Path) -> Optional[Callable[[Dict[str, Any], str], Optional[float]]]:
    score_py = bundle_dir / "judge" / "score_v2.py"
    if not score_py.exists():
        return None
    if str(bundle_dir) not in sys.path:
        sys.path.insert(0, str(bundle_dir))
    spec = importlib.util.spec_from_file_location("bundle_score_v2", score_py)
    mod = importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    spec.loader.exec_module(mod)
    return lambda q, out: mod.score(q, out)


def _build_inline_retriever(bundle_dir: Path, bundle: KR.CorpusBundle):
    """Build the exact dense + BM25 + cross-encoder batched Search worker from run_inline.py."""
    if str(bundle_dir) not in sys.path:
        sys.path.insert(0, str(bundle_dir))
    from conditions import base, v2  # type: ignore
    from runner import run_inline  # type: ignore

    world = v2.World(
        name="128k",
        docs=bundle.docs,
        chunks=bundle.chunks,
        retrieval={},
        queries=bundle.queries,
    )
    searcher = run_inline.Search(world)

    def _call(q: str) -> Tuple[List[str], Dict[str, float]]:
        ids, stage = searcher(q)
        return ids, {
            "search_queue_ms": stage.get("search_queue_ms", 0.0),
            "retrieve_ms": stage.get("candidate_batch_ms", 0.0),
            "rerank_ms": stage.get("rerank_batch_ms", 0.0),
            "search_batch_queries": float(stage.get("search_batch_queries", 1)),
        }

    return _call, searcher.close


def run_benchmark(args: argparse.Namespace) -> Dict[str, Any]:
    bundle_dir = Path(args.bundle_dir).resolve()
    out_dir = Path(args.out_dir).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    bundle = KR.load_crm_bundle(bundle_dir, max_sessions=args.max_sessions)
    router = KR.EntityRouter(bundle.docs, bundle.entities)
    if args.noisy_routing:
        bundle, nstats = KR.corrupt_bundle_noisy(bundle, router, seed=args.noise_seed)
        print(f"[noisy-routing] corrupted queries: {nstats}", flush=True)
    if args.tenant_scale > 1:
        bundle = KR.expand_bundle_tenants(bundle, scale=args.tenant_scale)
        print(f"[tenant-scale] expanded to {len(bundle.sessions)} isolated sessions", flush=True)
    scorer = _load_bundle_scorer(bundle_dir)

    close_retriever = lambda: None
    if args.retriever == "inline":
        retriever, close_retriever = _build_inline_retriever(bundle_dir, bundle)
    else:
        bm25 = KR.SimpleBM25(list(bundle.chunks.values()))
        retriever = lambda q: bm25.search(q, k=5)

    arms = [a.strip() for a in args.arms.split(",") if a.strip()]
    loads = [int(x.strip()) for x in args.loads.split(",") if x.strip()]

    cells: List[Dict[str, Any]] = []
    try:
        for block in range(1, args.sweeps + 1):
            order = list(arms) if (block % 2 == 1) else list(reversed(arms))
            for n in loads:
                for strat in order:
                    cell_path = out_dir / f"b{block}_n{n:02d}_{strat}.json"
                    if args.resume and cell_path.exists():
                        cell = json.loads(cell_path.read_text())
                        print(f"[resume] loaded {cell_path.name} ({cell['turns_per_min']:.1f} tpm)", flush=True)
                        cells.append(cell)
                        continue
                    print(f"[run] block={block} agents={n} strategy={strat} ...", flush=True)
                    cell = KR.run_cell(
                        bundle=bundle,
                        strategy=strat,
                        concurrency=n,
                        block=block,
                        base_url=args.base_url,
                        model=args.model,
                        retriever=retriever,
                        scorer=scorer,
                        max_tokens=args.max_tokens,
                        router=router,
                        wave_eviction=args.wave_eviction,
                    )
                    cell_path.write_text(json.dumps(cell, indent=2))
                    cells.append(cell)
                    s_one = KR.summarize_cells([cell])[f"{strat}@n{n}"]
                    print(
                        f"  done: {s_one['turns_per_min_mean']:.1f} tpm | "
                        f"p95_first={s_one['p95_first_pooled_s']:.2f}s | "
                        f"p95_end={s_one['p95_end_pooled_s']:.2f}s | "
                        f"gpu_kv={s_one['gpu_prefix_hit_pct']:.1f}% | "
                        f"ext_kv={s_one['ext_prefix_hit_pct']:.1f}% ({s_one['ext_prefix_hits_total']:.0f}) | "
                        f"prompt_toks={s_one['mean_prompt_tokens']:.0f} | "
                        f"acc={s_one['accuracy_all']}",
                        flush=True,
                    )
    finally:
        close_retriever()

    # Export flattened turns.jsonl
    with open(out_dir / "turns.jsonl", "w") as f:
        for c in cells:
            for t in c["turns"]:
                f.write(json.dumps(t) + "\n")

    summary = KR.summarize_cells(cells)
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2))

    # Compute paired McNemar across arms at each load
    mcnemar: Dict[str, Any] = {}
    for n in loads:
        by_arm: Dict[str, Dict[Tuple[int, str, int], float]] = {}
        for c in cells:
            if c["agents"] != n:
                continue
            d = by_arm.setdefault(c["strategy"], {})
            for t in c["turns"]:
                sc = KR.extract_score(t.get("score"))
                if sc is not None:
                    d[(t["block"], t["session"], t["turn"])] = sc
        arm_list = sorted(by_arm)
        for i in range(len(arm_list)):
            for j in range(i + 1, len(arm_list)):
                a, b = arm_list[i], arm_list[j]
                mcnemar[f"{a}_vs_{b}@n{n}"] = KR.mcnemar_exact(by_arm[a], by_arm[b])
    (out_dir / "mcnemar.json").write_text(json.dumps(mcnemar, indent=2))
    return {"summary": summary, "mcnemar": mcnemar}


def main() -> None:
    p = argparse.ArgumentParser(description="Run 3-arm routed-KV vs RAG vs 128K shared KV benchmark")
    p.add_argument("--bundle-dir", required=True, help="Extracted a100-inline-4608 directory")
    p.add_argument("--out-dir", required=True, help="Output directory for cell JSONs and summaries")
    p.add_argument("--base-url", default="http://127.0.0.1:8000", help="vLLM OpenAI base URL")
    p.add_argument("--model", default="Qwen/Qwen3-30B-A3B-Instruct-2507-FP8")
    p.add_argument("--arms", default="S-RAG,S-CTX,S-ROUTED")
    p.add_argument("--loads", default="1,8,32")
    p.add_argument("--sweeps", type=int, default=2)
    p.add_argument("--max-sessions", type=int, default=32, help="Sessions per cell (32 x 6 = 192 turns, matches run_inline)")
    p.add_argument("--max-tokens", type=int, default=384)
    p.add_argument("--retriever", choices=("inline", "bm25"), default="inline")
    p.add_argument("--noisy-routing", action="store_true", help="Inject Turn-1 typos and Turn 2-6 pronoun follow-ups")
    p.add_argument("--noise-seed", type=int, default=42)
    p.add_argument("--tenant-scale", type=int, default=1, help="Replicate sessions across N isolated tenants")
    p.add_argument("--wave-eviction", action="store_true", help="Run turn-by-turn waves to force GPU KV eviction")
    p.add_argument("--resume", action="store_true", help="Skip cells already saved in --out-dir")
    args = p.parse_args()
    res = run_benchmark(args)
    print(json.dumps(res["summary"], indent=2))


if __name__ == "__main__":
    main()
