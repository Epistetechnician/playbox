#!/usr/bin/env python3
"""CLI runner for SOTA benchmarks (LoCoMo + FinanceBench) comparing RAG, Conv-RAG, Shared KV, and Routed KV."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any, Dict, List

from playbox_steer.kv_sota_bench import (
    BM25ChunkRetriever,
    load_financebench,
    load_locomo,
    paired_mcnemar_sota,
    run_sota_cell,
)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--dataset", required=True, choices=["locomo", "financebench"])
    p.add_argument("--data-path", required=True)
    p.add_argument("--out-dir", required=True)
    p.add_argument("--base-url", default="http://127.0.0.1:8000")
    p.add_argument("--model", default="Qwen/Qwen3-30B-A3B-Instruct-2507-FP8")
    p.add_argument("--arms", default="S-RAG,S-CONV-RAG,S-ROUTED")
    p.add_argument("--concurrency", type=int, default=16)
    p.add_argument("--sweeps", type=int, default=1)
    p.add_argument("--max-tokens", type=int, default=128)
    p.add_argument("--top-k", type=int, default=5)
    p.add_argument("--max-per-conv", type=int, default=30)
    args = p.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    data_path = Path(args.data_path)
    dataset = (
        load_locomo(data_path, max_per_conv=args.max_per_conv)
        if args.dataset == "locomo"
        else load_financebench(data_path)
    )
    retriever = BM25ChunkRetriever(dataset.chunks, top_k=args.top_k)
    arms = [a.strip() for a in args.arms.split(",") if a.strip()]

    summaries: List[Dict[str, Any]] = []
    by_arm_turns: Dict[str, List[Dict[str, Any]]] = {a: [] for a in arms}

    for block in range(1, args.sweeps + 1):
        order = arms if block % 2 == 1 else list(reversed(arms))
        for arm in order:
            print(f"[{dataset.name}] block={block} n={args.concurrency} arm={arm} ...", flush=True)
            cell = run_sota_cell(
                dataset=dataset,
                strategy=arm,
                concurrency=args.concurrency,
                block=block,
                base_url=args.base_url,
                model=args.model,
                retriever=retriever,
                max_tokens=args.max_tokens,
            )
            s = cell["summary"]
            summaries.append(s)
            by_arm_turns[arm].extend(cell["turns"])
            cell_path = out_dir / f"b{block}_n{args.concurrency}_{arm}.json"
            cell_path.write_text(json.dumps(cell, indent=2), encoding="utf-8")
            print(
                f"  -> {s['turns_per_min']:.1f} tpm | p95_1st={s['p95_q_first_s']:.2f}s "
                f"| p95_end={s['p95_q_end_s']:.2f}s | tok={s['mean_prompt_tokens']:.0f} "
                f"| kv={s['cache_hit_pct']:.1f}% | f1={s['f1']*100:.1f}% | acc={s['accuracy']*100:.1f}% "
                f"| err={s['errors']}",
                flush=True,
            )

    mcnemar: Dict[str, Any] = {}
    if "S-ROUTED" in by_arm_turns:
        for base_arm in arms:
            if base_arm != "S-ROUTED":
                mcnemar[f"{base_arm}_vs_S-ROUTED"] = paired_mcnemar_sota(
                    by_arm_turns[base_arm], by_arm_turns["S-ROUTED"]
                )

    (out_dir / "summary.json").write_text(json.dumps(summaries, indent=2), encoding="utf-8")
    (out_dir / "mcnemar.json").write_text(json.dumps(mcnemar, indent=2), encoding="utf-8")
    print("McNemar:", json.dumps(mcnemar, indent=2), flush=True)


if __name__ == "__main__":
    main()
