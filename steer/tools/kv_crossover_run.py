#!/usr/bin/env python3
"""Experiment 1: 4K -> 64K Working-Set Crossover Law benchmark.

Sweeps entity dossier length L in {4096, 8192, 16384, 32768, 49152, 65536} tokens
and compares:
  - S-RAG: dense bi-encoder + cross-encoder reranker top-5 (~3.5K prompt)
  - S-CONV-RAG: LLM query rewriter + dense + cross-encoder reranker top-5
  - S-ROUTED: entity-routed working-set KV of length L + adaptive RAG fallback

Measures end-to-end throughput (turns/min), p95 q->first, p95 follow-up q->first,
p95 q->end, GPU/CPU KV cache hit %, and exact-match accuracy at each L to locate
the exact crossover threshold L* where routed KV meets RAG.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import math
import os
import random
import re
import time
import urllib.request
import uuid
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import numpy as np

from playbox_steer.kv_sota_bench import (
    BM25ChunkRetriever,
    DenseRerankChunkRetriever,
    SotaEntityRouter,
)

# ~4 chars per token approximation calibrated for English structured logs
CHARS_PER_TOKEN = 4


@dataclass
class CrossoverQuestion:
    qid: str
    entity_id: str
    session_id: str
    turn_idx: int
    question: str
    answer: str
    category: str


@dataclass
class CrossoverCorpus:
    target_tokens: int
    dossiers: Dict[str, str]
    chunks: List[Tuple[str, str]]
    sessions: List[List[CrossoverQuestion]]


def _make_filler_block(entity_id: str, idx: int, rng: random.Random) -> str:
    quarter = f"Q{(idx % 4) + 1}-202{2 + (idx % 4)}"
    region = rng.choice(["NA-East", "NA-West", "EMEA-North", "EMEA-South", "APAC-Core"])
    tier = rng.choice(["Enterprise", "Strategic", "Commercial", "Global"])
    rev = rng.randint(120, 9800) * 100
    tickets = rng.randint(2, 48)
    sla = rng.choice(["99.90%", "99.95%", "99.99%"])
    return (
        f"[Log #{idx:04d} | {quarter} | {region} | {tier}] "
        f"Account {entity_id} quarterly review recorded ${rev:,} recurring billing, "
        f"{tickets} resolved support interactions, SLA attainment {sla}, "
        f"standard compliance attestation renewed with zero Sev-1 incidents and "
        f"automated telemetry sync verified across primary and standby clusters.\n"
    )


def build_crossover_corpus(
    target_tokens: int,
    n_entities: int = 16,
    turns_per_session: int = 6,
    seed: int = 42,
) -> CrossoverCorpus:
    """Build deterministic entity dossiers padded to ~target_tokens with embedded facts."""
    rng = random.Random(seed + target_tokens)
    dossiers: Dict[str, str] = {}
    chunks: List[Tuple[str, str]] = []
    sessions: List[List[CrossoverQuestion]] = []

    target_chars = target_tokens * CHARS_PER_TOKEN
    entity_facts: List[Dict[str, str]] = []

    for i in range(n_entities):
        eid = f"ACME-{100 + i:03d}"
        name = f"Meridian Systems {i + 1:02d}"
        renewal = f"2026-{(i % 12) + 1:02d}-{10 + (i % 18):02d}"
        arr = f"${(250 + i * 37):,},000"
        csm = f"Jordan Patel-{i + 1:02d}"
        portal = f"portal-{100 + i}.meridian.example"
        code = f"KEY-{7000 + i * 13}"
        tier = ["Platinum", "Gold", "Diamond", "Titanium"][i % 4]
        facts = {
            "eid": eid,
            "name": name,
            "renewal": renewal,
            "arr": arr,
            "csm": csm,
            "portal": portal,
            "code": code,
            "tier": tier,
        }
        entity_facts.append(facts)

        # Place facts distributed across early, middle, and late positions in the dossier
        # so both KV attention and RAG chunking must find facts across the full length L.
        header = (
            f"=== ACCOUNT DOSSIER: {name} ({eid}) ===\n"
            f"Account Name: {name}\n"
            f"Account ID: {eid}\n"
            f"Support Tier: {tier}\n"
            f"Primary CSM: {csm}\n\n"
        )
        mid_fact = (
            f"\n[CONTRACT SUMMARY | {name} ({eid})]\n"
            f"Annual Recurring Revenue (ARR): {arr}\n"
            f"Contract Renewal Date: {renewal}\n\n"
        )
        late_fact = (
            f"\n[SECURITY & PORTAL RECORD | {name} ({eid})]\n"
            f"Procurement Portal: {portal}\n"
            f"Escalation Authorization Code: {code}\n\n"
        )

        fixed_len = len(header) + len(mid_fact) + len(late_fact)
        filler_budget = max(0, target_chars - fixed_len)
        half_budget = filler_budget // 2

        part1_blocks: List[str] = []
        cur = 0
        idx = 0
        while cur < half_budget:
            blk = _make_filler_block(name, idx, rng)
            part1_blocks.append(blk)
            cur += len(blk)
            idx += 1

        part2_blocks: List[str] = []
        cur = 0
        while cur < half_budget:
            blk = _make_filler_block(name, idx, rng)
            part2_blocks.append(blk)
            cur += len(blk)
            idx += 1

        full_text = header + "".join(part1_blocks) + mid_fact + "".join(part2_blocks) + late_fact
        dossiers[eid] = full_text

        # Split into ~2,800-char (~700-token) chunks so top-5 RAG is ~3.5K tokens across all L
        chunk_size = 2800
        for ci, start in enumerate(range(0, len(full_text), chunk_size)):
            chunk_text = f"[{name} ({eid}) chunk {ci}]\n" + full_text[start : start + chunk_size]
            chunks.append((f"{eid}_c{ci}", chunk_text))

    # Build 6-turn sessions per entity:
    # Turn 1: explicit entity name (early fact)
    # Turn 2: implicit pronoun follow-up (middle fact: ARR)
    # Turn 3: implicit pronoun follow-up (middle fact: renewal date)
    # Turn 4: explicit entity name (late fact: portal)
    # Turn 5: implicit pronoun follow-up (late fact: escalation code)
    # Turn 6: cross-entity lookup to another account (triggers adaptive RAG fallback)
    for i, f in enumerate(entity_facts):
        other = entity_facts[(i + 1) % n_entities]
        qs = [
            CrossoverQuestion(
                qid=f"L{target_tokens}_S{i:02d}_T1",
                entity_id=f["eid"],
                session_id=f"S{i:02d}",
                turn_idx=1,
                question=f"Who is the Primary CSM for {f['name']} ({f['eid']})?",
                answer=f["csm"],
                category="explicit_early",
            ),
            CrossoverQuestion(
                qid=f"L{target_tokens}_S{i:02d}_T2",
                entity_id=f["eid"],
                session_id=f"S{i:02d}",
                turn_idx=2,
                question="What is the Annual Recurring Revenue (ARR) for this account?",
                answer=f["arr"],
                category="implicit_mid",
            ),
            CrossoverQuestion(
                qid=f"L{target_tokens}_S{i:02d}_T3",
                entity_id=f["eid"],
                session_id=f"S{i:02d}",
                turn_idx=3,
                question="What is this account's Contract Renewal Date?",
                answer=f["renewal"],
                category="implicit_mid",
            ),
            CrossoverQuestion(
                qid=f"L{target_tokens}_S{i:02d}_T4",
                entity_id=f["eid"],
                session_id=f"S{i:02d}",
                turn_idx=4,
                question=f"What is the Procurement Portal URL for {f['name']}?",
                answer=f["portal"],
                category="explicit_late",
            ),
            CrossoverQuestion(
                qid=f"L{target_tokens}_S{i:02d}_T5",
                entity_id=f["eid"],
                session_id=f"S{i:02d}",
                turn_idx=5,
                question="What is the Escalation Authorization Code for this account?",
                answer=f["code"],
                category="implicit_late",
            ),
            CrossoverQuestion(
                qid=f"L{target_tokens}_S{i:02d}_T6",
                entity_id=other["eid"],
                session_id=f"S{i:02d}",
                turn_idx=6,
                question=f"Compare with {other['name']} ({other['eid']}): what is its Support Tier?",
                answer=other["tier"],
                category="cross_entity",
            ),
        ][:turns_per_session]
        sessions.append(qs)

    return CrossoverCorpus(
        target_tokens=target_tokens,
        dossiers=dossiers,
        chunks=chunks,
        sessions=sessions,
    )


def _fetch_vllm_metrics(base_url: str) -> Dict[str, float]:
    out: Dict[str, float] = {}
    try:
        with urllib.request.urlopen(f"{base_url.rstrip('/')}/metrics", timeout=5) as resp:
            text = resp.read().decode("utf-8", errors="ignore")
    except Exception:
        return out
    for line in text.splitlines():
        if line.startswith("#") or not line.strip():
            continue
        for key in (
            "vllm:prefix_cache_hits_total",
            "vllm:prefix_cache_queries_total",
            "vllm:external_prefix_cache_hits_total",
            "vllm:external_prefix_cache_queries_total",
        ):
            if line.startswith(key):
                parts = line.rsplit(" ", 1)
                if len(parts) == 2:
                    try:
                        out[key] = out.get(key, 0.0) + float(parts[1])
                    except ValueError:
                        pass
    return out


async def _chat_call(
    session: Any,
    base_url: str,
    model: str,
    messages: List[Dict[str, str]],
    max_tokens: int,
    seed: int,
    extra_body: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    payload: Dict[str, Any] = {
        "model": model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "seed": seed,
        "stream": True,
        "stream_options": {"include_usage": True},
    }
    if extra_body:
        payload.update(extra_body)
    t0 = time.perf_counter()
    t_first: Optional[float] = None
    chunks_out: List[str] = []
    usage: Dict[str, Any] = {}
    err: Optional[str] = None
    try:
        async with session.post(f"{base_url.rstrip('/')}/v1/chat/completions", json=payload, timeout=300) as resp:
            if resp.status != 200:
                err = f"HTTP {resp.status}: {(await resp.text())[:200]}"
            else:
                async for raw in resp.content:
                    line = raw.decode("utf-8", errors="ignore").strip()
                    if not line.startswith("data:"):
                        continue
                    data_s = line[5:].strip()
                    if data_s == "[DONE]":
                        break
                    try:
                        obj = json.loads(data_s)
                    except json.JSONDecodeError:
                        continue
                    if obj.get("usage"):
                        usage = obj["usage"]
                    choices = obj.get("choices") or []
                    if choices:
                        delta = choices[0].get("delta", {})
                        tok = delta.get("content") or ""
                        if tok:
                            if t_first is None:
                                t_first = time.perf_counter()
                            chunks_out.append(tok)
    except Exception as exc:
        err = repr(exc)
    t_end = time.perf_counter()
    return {
        "text": "".join(chunks_out).strip(),
        "ttft_s": (t_first - t0) if t_first is not None else (t_end - t0),
        "total_s": t_end - t0,
        "prompt_tokens": int(usage.get("prompt_tokens") or 0),
        "completion_tokens": int(usage.get("completion_tokens") or 0),
        "error": err,
    }


def _pct(vals: List[float], q: float) -> float:
    if not vals:
        return 0.0
    return float(np.percentile(vals, q))


async def run_cell(
    corpus: CrossoverCorpus,
    arm: str,
    retriever_fn: Any,
    router: SotaEntityRouter,
    base_url: str,
    model: str,
    concurrency: int,
    max_tokens: int,
    seed: int,
    disable_thinking: bool,
) -> Tuple[Dict[str, Any], List[Dict[str, Any]]]:
    import aiohttp

    salt = f"[bench-salt:{uuid.uuid4().hex[:12]}]"
    extra_body = {"chat_template_kwargs": {"enable_thinking": False}} if disable_thinking else None

    m_before = _fetch_vllm_metrics(base_url)
    sem = asyncio.Semaphore(concurrency)
    results: List[Dict[str, Any]] = []
    t_wall0 = time.perf_counter()

    async def _run_session(sess_qs: List[CrossoverQuestion], http_sess: Any) -> None:
        active_entity: Optional[str] = None
        history: List[Tuple[str, str]] = []
        async with sem:
            for q in sess_qs:
                t_q0 = time.perf_counter()
                rewrite_ms = 0.0
                retrieve_ms = 0.0
                route_mode = arm
                fallback = False

                if arm == "S-RAG":
                    t_r0 = time.perf_counter()
                    docs, _ = await asyncio.to_thread(retriever_fn, q.question)
                    retrieve_ms = (time.perf_counter() - t_r0) * 1000.0
                    ctx = "\n\n".join(docs)
                    messages = [
                        {
                            "role": "system",
                            "content": f"{salt} Answer concisely with the exact value from the context. End with 'Answer: <value>'.",
                        },
                        {"role": "user", "content": f"Context:\n{ctx}\n\nQuestion: {q.question}"},
                    ]
                elif arm == "S-CONV-RAG":
                    search_q = q.question
                    if history:
                        t_rw0 = time.perf_counter()
                        hist_txt = "\n".join(f"Q: {hq}\nA: {ha}" for hq, ha in history[-3:])
                        rw_msgs = [
                            {
                                "role": "system",
                                "content": "Rewrite the follow-up question into a standalone question including the account name/ID. Output only the rewritten question.",
                            },
                            {"role": "user", "content": f"History:\n{hist_txt}\n\nFollow-up: {q.question}"},
                        ]
                        rw = await _chat_call(http_sess, base_url, model, rw_msgs, 64, seed, extra_body)
                        rewrite_ms = (time.perf_counter() - t_rw0) * 1000.0
                        if rw["text"] and not rw["error"]:
                            search_q = rw["text"]
                    t_r0 = time.perf_counter()
                    docs, _ = await asyncio.to_thread(retriever_fn, search_q)
                    retrieve_ms = (time.perf_counter() - t_r0) * 1000.0
                    ctx = "\n\n".join(docs)
                    messages = [
                        {
                            "role": "system",
                            "content": f"{salt} Answer concisely with the exact value from the context. End with 'Answer: <value>'.",
                        },
                        {"role": "user", "content": f"Context:\n{ctx}\n\nQuestion: {q.question}"},
                    ]
                elif arm == "S-ROUTED":
                    matched, rmode = router.resolve(q.question, active_entity=active_entity)
                    if rmode == "entity_hit" and (active_entity is None or matched == active_entity):
                        active_entity = matched
                        route_mode = "entity"
                    elif rmode == "entity_hit" and active_entity is not None and matched != active_entity:
                        # Cross-entity drift: keep active_entity prefix warm and attach RAG fallback
                        fallback = True
                        route_mode = "cross_fallback"
                    elif rmode == "session_carry":
                        route_mode = "carry"
                    else:
                        fallback = True
                        route_mode = "rag_fallback"

                    dossier = corpus.dossiers.get(active_entity or "", "")
                    sys_prompt = (
                        f"{salt} You are an enterprise account assistant. Answer concisely with the exact value from the dossier"
                        f" (or supplemental context if provided). End with 'Answer: <value>'.\n\n{dossier}"
                    )
                    user_prompt = q.question
                    if fallback:
                        t_r0 = time.perf_counter()
                        docs, _ = await asyncio.to_thread(retriever_fn, q.question)
                        retrieve_ms = (time.perf_counter() - t_r0) * 1000.0
                        user_prompt = "Supplemental context:\n" + "\n\n".join(docs) + f"\n\nQuestion: {q.question}"
                    messages = [
                        {"role": "system", "content": sys_prompt},
                        {"role": "user", "content": user_prompt},
                    ]
                else:
                    raise ValueError(f"Unknown arm: {arm}")

                pre_llm_s = time.perf_counter() - t_q0
                resp = await _chat_call(http_sess, base_url, model, messages, max_tokens, seed, extra_body)
                q_first_s = pre_llm_s + resp["ttft_s"]
                q_end_s = time.perf_counter() - t_q0
                ok = (not resp["error"]) and (q.answer.lower() in resp["text"].lower())
                history.append((q.question, resp["text"][:200]))
                results.append(
                    {
                        "qid": q.qid,
                        "session_id": q.session_id,
                        "turn_idx": q.turn_idx,
                        "category": q.category,
                        "route_mode": route_mode,
                        "fallback": fallback,
                        "rewrite_ms": rewrite_ms,
                        "retrieve_ms": retrieve_ms,
                        "q_first_s": q_first_s,
                        "q_end_s": q_end_s,
                        "prompt_tokens": resp["prompt_tokens"],
                        "completion_tokens": resp["completion_tokens"],
                        "correct": ok,
                        "error": resp["error"],
                        "answer_gold": q.answer,
                        "answer_pred": resp["text"][:160],
                    }
                )

    conn = aiohttp.TCPConnector(limit=concurrency * 2)
    async with aiohttp.ClientSession(connector=conn) as http_sess:
        await asyncio.gather(*[_run_session(s, http_sess) for s in corpus.sessions])

    wall_s = time.perf_counter() - t_wall0
    m_after = _fetch_vllm_metrics(base_url)

    gpu_h = m_after.get("vllm:prefix_cache_hits_total", 0.0) - m_before.get("vllm:prefix_cache_hits_total", 0.0)
    gpu_q = m_after.get("vllm:prefix_cache_queries_total", 0.0) - m_before.get("vllm:prefix_cache_queries_total", 0.0)
    ext_h = m_after.get("vllm:external_prefix_cache_hits_total", 0.0) - m_before.get("vllm:external_prefix_cache_hits_total", 0.0)
    ext_q = m_after.get("vllm:external_prefix_cache_queries_total", 0.0) - m_before.get("vllm:external_prefix_cache_queries_total", 0.0)

    n = len(results)
    errs = sum(1 for r in results if r["error"])
    t2_first = [r["q_first_s"] for r in results if r["turn_idx"] >= 2]
    summary = {
        "target_tokens": corpus.target_tokens,
        "arm": arm,
        "concurrency": concurrency,
        "n_turns": n,
        "errors": errs,
        "wall_s": round(wall_s, 3),
        "turns_per_min": round(n / (wall_s / 60.0), 2) if wall_s > 0 else 0.0,
        "p95_q_first_s": round(_pct([r["q_first_s"] for r in results], 95), 3),
        "p95_t2_first_s": round(_pct(t2_first, 95), 3),
        "p95_q_end_s": round(_pct([r["q_end_s"] for r in results], 95), 3),
        "mean_prompt_tokens": round(float(np.mean([r["prompt_tokens"] for r in results])), 1) if results else 0.0,
        "mean_completion_tokens": round(float(np.mean([r["completion_tokens"] for r in results])), 1) if results else 0.0,
        "mean_rewrite_ms": round(float(np.mean([r["rewrite_ms"] for r in results])), 1) if results else 0.0,
        "mean_retrieve_ms": round(float(np.mean([r["retrieve_ms"] for r in results])), 1) if results else 0.0,
        "gpu_prefix_hit_pct": round((gpu_h / gpu_q) * 100.0, 2) if gpu_q > 0 else 0.0,
        "ext_prefix_hit_pct": round((ext_h / ext_q) * 100.0, 2) if ext_q > 0 else 0.0,
        "ext_prefix_hits_total": int(ext_h),
        "fallback_rate": round(sum(1 for r in results if r["fallback"]) / max(1, n), 4),
        "accuracy": round(sum(1 for r in results if r["correct"]) / max(1, n), 4),
    }
    return summary, results


def main() -> None:
    ap = argparse.ArgumentParser(description="4K -> 64K Working-Set Crossover Benchmark")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--base-url", default="http://127.0.0.1:8000")
    ap.add_argument("--model", required=True)
    ap.add_argument("--lengths", default="4096,8192,16384,32768,49152,65536")
    ap.add_argument("--arms", default="S-RAG,S-CONV-RAG,S-ROUTED")
    ap.add_argument("--n-entities", type=int, default=16)
    ap.add_argument("--turns-per-session", type=int, default=6)
    ap.add_argument("--concurrency", type=int, default=8)
    ap.add_argument("--max-tokens", type=int, default=128)
    ap.add_argument("--retriever", choices=["bm25", "dense_rerank"], default="dense_rerank")
    ap.add_argument("--embed-model", default="BAAI/bge-small-en-v1.5")
    ap.add_argument("--rerank-model", default="BAAI/bge-reranker-v2-m3")
    ap.add_argument("--retriever-device", default="cpu")
    ap.add_argument("--cand-k", type=int, default=20)
    ap.add_argument("--top-k", type=int, default=5)
    ap.add_argument("--disable-thinking", action="store_true")
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    lengths = [int(x.strip()) for x in args.lengths.split(",") if x.strip()]
    arms = [x.strip() for x in args.arms.split(",") if x.strip()]

    shared_embed_fn = None
    shared_rerank_fn = None
    if args.retriever == "dense_rerank":
        import torch
        from sentence_transformers import CrossEncoder, SentenceTransformer

        mk = {"dtype": torch.float16} if args.retriever_device.startswith("cuda") else {}
        enc = SentenceTransformer(args.embed_model, device=args.retriever_device, model_kwargs=mk)
        rr = CrossEncoder(args.rerank_model, device=args.retriever_device, model_kwargs=mk)
        shared_embed_fn = lambda xs: enc.encode(xs, normalize_embeddings=True, batch_size=64, show_progress_bar=False).tolist()
        shared_rerank_fn = lambda pairs: [float(x) for x in rr.predict(pairs, batch_size=32, show_progress_bar=False)]

    all_summaries: List[Dict[str, Any]] = []
    for L in lengths:
        print(f"\n=== Building corpus L={L} tokens ({args.n_entities} entities) ===", flush=True)
        corpus = build_crossover_corpus(L, n_entities=args.n_entities, turns_per_session=args.turns_per_session, seed=args.seed)
        aliases = {}
        for i in range(args.n_entities):
            eid = f"ACME-{100 + i:03d}"
            aliases[eid] = [eid, f"Meridian Systems {i + 1:02d}"]
        router = SotaEntityRouter(corpus.dossiers, aliases)

        if args.retriever == "dense_rerank":
            retriever_fn = DenseRerankChunkRetriever(
                corpus.chunks,
                embed_model=args.embed_model,
                rerank_model=args.rerank_model,
                device=args.retriever_device,
                cand_k=args.cand_k,
                top_k=args.top_k,
                embed_fn=shared_embed_fn,
                rerank_fn=shared_rerank_fn,
            )
        else:
            retriever_fn = BM25ChunkRetriever(corpus.chunks, top_k=args.top_k)

        for arm in arms:
            print(f"Running L={L} arm={arm} n={args.concurrency} ...", flush=True)
            summary, raw = asyncio.run(
                run_cell(
                    corpus=corpus,
                    arm=arm,
                    retriever_fn=retriever_fn,
                    router=router,
                    base_url=args.base_url,
                    model=args.model,
                    concurrency=args.concurrency,
                    max_tokens=args.max_tokens,
                    seed=args.seed,
                    disable_thinking=args.disable_thinking,
                )
            )
            all_summaries.append(summary)
            (out_dir / f"L{L}_{arm}.json").write_text(json.dumps(raw, indent=2))
            (out_dir / "summary.json").write_text(json.dumps(all_summaries, indent=2))
            print(
                f"  -> L={L} {arm}: {summary['turns_per_min']} tpm | "
                f"p95_q1={summary['p95_q_first_s']}s t2={summary['p95_t2_first_s']}s end={summary['p95_q_end_s']}s | "
                f"tok={summary['mean_prompt_tokens']} gpu_hit={summary['gpu_prefix_hit_pct']}% cpu_hit={summary['ext_prefix_hit_pct']}% | "
                f"acc={summary['accuracy']*100:.1f}% err={summary['errors']}",
                flush=True,
            )

    print(f"\nDONE. Saved {len(all_summaries)} cells to {out_dir / 'summary.json'}", flush=True)


if __name__ == "__main__":
    main()
