#!/usr/bin/env python3
"""Experiment 3: Mutating-State Delta-Framed Entity KV + Breaking the 64K KV Wall.

Runs two controlled end-to-end benchmarks against a live vLLM server:

Sub-Experiment A (Mutating-State Live Writes, L ~= 12K tokens/entity, n=16):
  Live mutations overwrite entity fields (SLA, ARR, Renewal Date) mid-session.
  Compares:
    - S-NAIVE-REWRITE : mutates the dossier in-place (invalidates prefix cache)
    - S-DELTA-KV      : immutable base snapshot + append-only [DELTA #k] frames
    - S-DELTA-COMPACT : append-only deltas + cost-aware compaction (max_stale=2)
    - S-RAG-LIVE      : stateless chunked RAG over updated entity text

Sub-Experiment B (Breaking the 64K KV Capacity Wall, L ~= 64K tokens/entity, n=16):
  Oversized 64K-token dossiers with both deep-buried clauses and live mutations.
  Compares:
    - S-FULL-64K-KV    : pins full 64K dossier in KV (hits decode/KV capacity wall)
    - S-RAG-64K        : stateless chunked RAG over 64K dossiers
    - S-HYBRID-HIER-KV : Auto-Crossover compact ~2.5K skeleton + live deltas in KV
                         + entity-scoped cold-archive RAG top-3
"""
from __future__ import annotations

import argparse
import asyncio
import json
import random
import re
import time
import urllib.request
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import numpy as np

from playbox_steer.kv_delta import (
    AutoCrossoverPolicy,
    DELTA_OVERRIDE_RULE,
    DeltaFramedEntity,
    prefix_preserved_ratio,
)
from playbox_steer.kv_sota_bench import BM25ChunkRetriever, SotaEntityRouter

CHARS_PER_TOKEN = 4


@dataclass
class MutationStep:
    turn_idx: int
    field_name: str
    new_value: str
    note: str


@dataclass
class DeltaTurn:
    qid: str
    entity_id: str
    session_id: str
    turn_idx: int
    question: str
    expected: str
    category: str  # "base_fact" | "mutated_fact" | "deep_clause" | "implicit_followup"
    mutation_before: Optional[MutationStep] = None


@dataclass
class EntityFixture:
    entity_id: str
    name: str
    base_dossier: str
    facts: Dict[str, str]


def _make_filler(entity_id: str, idx: int, rng: random.Random) -> str:
    qtr = f"Q{(idx % 4) + 1}-202{2 + (idx % 4)}"
    region = rng.choice(["NA-East", "NA-West", "EMEA-North", "APAC-Core"])
    rev = rng.randint(150, 9500) * 100
    tickets = rng.randint(3, 45)
    return (
        f"[Audit #{idx:04d} | {qtr} | {region}] Account {entity_id} routine review logged "
        f"${rev:,} recurring billing, {tickets} closed support tickets, automated cluster "
        f"failover check passed with zero Sev-1 incidents and SOC2 telemetry verified.\n"
    )


def build_mutating_workload(
    n_entities: int = 16,
    sessions_per_entity: int = 2,
    target_tokens: int = 12000,
    seed: int = 42,
) -> Tuple[List[EntityFixture], List[List[DeltaTurn]]]:
    """Build Sub-Experiment A: 12K dossiers with mid-session live field mutations."""
    rng = random.Random(seed)
    fixtures: List[EntityFixture] = []
    sessions: List[List[DeltaTurn]] = []
    target_chars = target_tokens * CHARS_PER_TOKEN

    for i in range(n_entities):
        eid = f"ENT-{200 + i:03d}"
        name = f"Helios Dynamics {i + 1:02d}"
        sla0 = "99.90%"
        arr0 = f"${400 + i * 25:,},000"
        csm0 = f"Alex Rivera-{i + 1:02d}"
        code0 = f"TOKEN-{8100 + i * 17}"
        portal0 = f"app-{200 + i}.helios.example"

        header = (
            f"=== ACCOUNT DOSSIER: {name} ({eid}) ===\n"
            f"Account Name: {name}\n"
            f"Account ID: {eid}\n"
            f"SLA Target: {sla0}\n"
            f"Annual Recurring Revenue: {arr0}\n"
            f"Customer Success Manager: {csm0}\n"
            f"Support Portal: {portal0}\n"
        )
        tail_fact = f"\n=== SECURITY & BILLING APPENDIX ===\nAPI Key Prefix: {code0}\n"

        blocks: List[str] = [header]
        idx = 0
        while sum(len(b) for b in blocks) + len(tail_fact) < target_chars:
            blocks.append(_make_filler(eid, idx, rng))
            idx += 1
        blocks.append(tail_fact)
        dossier = "".join(blocks)
        fixtures.append(
            EntityFixture(
                entity_id=eid,
                name=name,
                base_dossier=dossier,
                facts={"sla0": sla0, "arr0": arr0, "csm0": csm0, "code0": code0, "portal0": portal0},
            )
        )

        for s in range(sessions_per_entity):
            sid = f"mut_{eid}_s{s}"
            sla1 = f"99.{95 + ((i + s) % 4)}%"
            arr1 = f"${650 + (i * 31 + s * 40):,},000"
            sla2 = f"99.99{1 + ((i + s) % 8)}%"
            turns = [
                DeltaTurn(f"{sid}_t0", eid, sid, 0, f"For {name} ({eid}), what is the current SLA Target?", sla0, "base_fact"),
                DeltaTurn(f"{sid}_t1", eid, sid, 1, "What is their API Key Prefix?", code0, "implicit_followup"),
                DeltaTurn(
                    f"{sid}_t2", eid, sid, 2,
                    f"After the live contract update for {name}, what is their new SLA Target?",
                    sla1, "mutated_fact",
                    mutation_before=MutationStep(2, "SLA Target", sla1, "Tier upgrade"),
                ),
                DeltaTurn(f"{sid}_t3", eid, sid, 3, "And who is their Customer Success Manager?", csm0, "implicit_followup"),
                DeltaTurn(
                    f"{sid}_t4", eid, sid, 4,
                    f"What is the updated Annual Recurring Revenue for {name}?",
                    arr1, "mutated_fact",
                    mutation_before=MutationStep(4, "Annual Recurring Revenue", arr1, "Seat expansion"),
                ),
                DeltaTurn(f"{sid}_t5", eid, sid, 5, "What is their Support Portal hostname?", portal0, "implicit_followup"),
                DeltaTurn(
                    f"{sid}_t6", eid, sid, 6,
                    "After the latest executive amendment, what is their current SLA Target now?",
                    sla2, "mutated_fact",
                    mutation_before=MutationStep(6, "SLA Target", sla2, "Executive amendment"),
                ),
                DeltaTurn(f"{sid}_t7", eid, sid, 7, "Confirm their current Annual Recurring Revenue after all updates.", arr1, "mutated_fact"),
            ]
            sessions.append(turns)

    return fixtures, sessions


def build_64k_wall_workload(
    n_entities: int = 8,
    sessions_per_entity: int = 2,
    target_tokens: int = 64000,
    seed: int = 99,
) -> Tuple[List[EntityFixture], List[List[DeltaTurn]]]:
    """Build Sub-Experiment B: 64K dossiers with deep clauses + live mutations."""
    rng = random.Random(seed)
    fixtures: List[EntityFixture] = []
    sessions: List[List[DeltaTurn]] = []
    target_chars = target_tokens * CHARS_PER_TOKEN

    for i in range(n_entities):
        eid = f"MEGA-{300 + i:03d}"
        name = f"Titan Enterprise {i + 1:02d}"
        sla0 = "99.90%"
        arr0 = f"${2_100 + i * 180:,},000"
        hotline = f"EXT-{9100 + i * 29}"
        escrow = f"ESCROW-{4400 + i * 19}"
        auditor = f"Vanguard Audit-{i + 1:02d}"

        header = (
            f"=== ACCOUNT DOSSIER: {name} ({eid}) ===\n"
            f"Account Name: {name}\n"
            f"Account ID: {eid}\n"
            f"SLA Target: {sla0}\n"
            f"Annual Recurring Revenue: {arr0}\n"
            f"External Auditor: {auditor}\n"
        )
        blocks: List[str] = [header]
        idx = 0
        placed_mid = False
        placed_late = False
        while sum(len(b) for b in blocks) < target_chars:
            cur = sum(len(b) for b in blocks)
            if not placed_mid and cur >= int(target_chars * 0.45):
                blocks.append(
                    f"\n=== SPECIAL CLAUSE 42: DISASTER RECOVERY ===\n"
                    f"Account {name} ({eid}) Emergency Escalation Hotline is {hotline}.\n"
                )
                placed_mid = True
            if not placed_late and cur >= int(target_chars * 0.82):
                blocks.append(
                    f"\n=== SPECIAL CLAUSE 88: SOURCE CODE ESCROW ===\n"
                    f"Account {name} ({eid}) Escrow Vault Identifier is {escrow}.\n"
                )
                placed_late = True
            blocks.append(_make_filler(eid, idx, rng))
            idx += 1

        dossier = "".join(blocks)
        fixtures.append(
            EntityFixture(
                entity_id=eid,
                name=name,
                base_dossier=dossier,
                facts={"sla0": sla0, "arr0": arr0, "hotline": hotline, "escrow": escrow, "auditor": auditor},
            )
        )

        for s in range(sessions_per_entity):
            sid = f"wall_{eid}_s{s}"
            sla1 = f"99.99{2 + ((i + s) % 7)}%"
            turns = [
                DeltaTurn(f"{sid}_t0", eid, sid, 0, f"For {name} ({eid}), who is their External Auditor?", auditor, "base_fact"),
                DeltaTurn(f"{sid}_t1", eid, sid, 1, f"What is the Emergency Escalation Hotline for {name}?", hotline, "deep_clause"),
                DeltaTurn(
                    f"{sid}_t2", eid, sid, 2,
                    "After the live contract update, what is their new SLA Target?",
                    sla1, "mutated_fact",
                    mutation_before=MutationStep(2, "SLA Target", sla1, "Enterprise SLA amendment"),
                ),
                DeltaTurn(f"{sid}_t3", eid, sid, 3, f"What is the Escrow Vault Identifier for {name}?", escrow, "deep_clause"),
                DeltaTurn(f"{sid}_t4", eid, sid, 4, "What is their Annual Recurring Revenue?", arr0, "implicit_followup"),
                DeltaTurn(f"{sid}_t5", eid, sid, 5, "Re-confirm their current SLA Target after the update.", sla1, "mutated_fact"),
            ]
            sessions.append(turns)

    return fixtures, sessions


def _scrape_vllm_kv_metrics(base_url: str) -> Dict[str, float]:
    out: Dict[str, float] = {}
    try:
        with urllib.request.urlopen(f"{base_url.rstrip('/')}/metrics", timeout=5.0) as r:
            text = r.read().decode("utf-8", errors="ignore")
    except Exception:
        return out
    for line in text.splitlines():
        if line.startswith("#") or not line.strip():
            continue
        for key in (
            "vllm:prefix_cache_hits_total",
            "vllm:prefix_cache_queries_total",
            "vllm:gpu_prefix_cache_hits_total",
            "vllm:gpu_prefix_cache_queries_total",
        ):
            if line.startswith(key):
                parts = line.split()
                try:
                    out[key] = out.get(key, 0.0) + float(parts[-1])
                except ValueError:
                    pass
    return out


def _kv_hit_pct(before: Dict[str, float], after: Dict[str, float]) -> float:
    for h_k, q_k in (
        ("vllm:prefix_cache_hits_total", "vllm:prefix_cache_queries_total"),
        ("vllm:gpu_prefix_cache_hits_total", "vllm:gpu_prefix_cache_queries_total"),
    ):
        dh = after.get(h_k, 0.0) - before.get(h_k, 0.0)
        dq = after.get(q_k, 0.0) - before.get(q_k, 0.0)
        if dq > 0:
            return round(100.0 * dh / dq, 2)
    return 0.0


def _score_exact(pred: str, expected: str) -> int:
    norm_p = re.sub(r"\s+", " ", (pred or "").lower())
    norm_e = re.sub(r"\s+", " ", expected.lower())
    if norm_e in norm_p:
        return 1
    # Normalize currency/thousands formatting (e.g. "$650,000" == "650000")
    strip_p = re.sub(r"[\$,]", "", norm_p)
    strip_e = re.sub(r"[\$,]", "", norm_e)
    return 1 if (strip_e and strip_e in strip_p) else 0


async def _call_vllm_stream(
    base_url: str,
    model: str,
    messages: List[Dict[str, str]],
    max_tokens: int = 48,
    timeout_s: float = 180.0,
) -> Dict[str, Any]:
    import httpx

    payload = {
        "model": model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "stream": True,
        "chat_template_kwargs": {"enable_thinking": False},
    }
    t_start = time.perf_counter()
    t_first: Optional[float] = None
    chunks: List[str] = []
    err: Optional[str] = None
    try:
        async with httpx.AsyncClient(timeout=timeout_s) as client:
            async with client.stream("POST", f"{base_url.rstrip('/')}/v1/chat/completions", json=payload) as resp:
                resp.raise_for_status()
                async for line in resp.aiter_lines():
                    if not line or not line.startswith("data:"):
                        continue
                    data_s = line[5:].strip()
                    if data_s == "[DONE]":
                        break
                    try:
                        obj = json.loads(data_s)
                    except json.JSONDecodeError:
                        continue
                    delta = ((obj.get("choices") or [{}])[0].get("delta") or {}).get("content")
                    if delta:
                        if t_first is None:
                            t_first = time.perf_counter()
                        chunks.append(delta)
    except Exception as exc:
        err = f"{type(exc).__name__}: {exc}"
    t_end = time.perf_counter()
    return {
        "text": "".join(chunks).strip(),
        "q_to_first_s": round((t_first - t_start) if t_first is not None else (t_end - t_start), 4),
        "q_to_end_s": round(t_end - t_start, 4),
        "error": err,
    }


def _mutate_in_place(dossier: str, field_name: str, new_value: str) -> str:
    pat = re.compile(rf"^({re.escape(field_name)}\s*:\s*).*$", flags=re.MULTILINE)
    if pat.search(dossier):
        return pat.sub(rf"\g<1>{new_value}", dossier, count=1)
    return f"{field_name}: {new_value}\n" + dossier


def _chunk_text(entity_id: str, text: str, chunk_chars: int = 1600) -> List[Tuple[str, str]]:
    lines = text.splitlines()
    chunks: List[Tuple[str, str]] = []
    buf: List[str] = []
    blen = 0
    cid = 0
    for ln in lines:
        buf.append(ln)
        blen += len(ln) + 1
        if blen >= chunk_chars:
            chunks.append((f"{entity_id}:c{cid}", "\n".join(buf)))
            cid += 1
            buf = []
            blen = 0
    if buf:
        chunks.append((f"{entity_id}:c{cid}", "\n".join(buf)))
    return chunks


async def run_arm(
    arm_name: str,
    base_url: str,
    model: str,
    fixtures: List[EntityFixture],
    sessions: List[List[DeltaTurn]],
    concurrency: int = 16,
    max_tokens: int = 48,
) -> Dict[str, Any]:
    """Execute one serving arm across concurrent sessions with live mutations."""
    fix_map = {f.entity_id: f for f in fixtures}
    run_nonce = f"{int(time.time())}"
    sys_preamble = (
        f"[ARM:{arm_name} | RUN:{run_nonce}] "
        "You are a precise enterprise assistant. Answer concisely with the exact requested value only."
    )
    policy = AutoCrossoverPolicy(l_full_max_tokens=13200, l_hybrid_switch_tokens=24000, l_wall_tokens=48000)

    # Pre-warm static corpus chunks for RAG arms
    base_chunks: List[Tuple[str, str]] = []
    for f in fixtures:
        base_chunks.extend(_chunk_text(f.entity_id, f.base_dossier))
    global_rag = BM25ChunkRetriever(base_chunks, top_k=5) if "RAG" in arm_name else None
    entity_chunk_cache: Dict[str, List[Tuple[str, str]]] = {
        f.entity_id: _chunk_text(f.entity_id, f.base_dossier) for f in fixtures
    }

    sem = asyncio.Semaphore(concurrency)
    turn_logs: List[Dict[str, Any]] = []
    preserved_ratios: List[float] = []
    compactions_total = 0

    m_before = _scrape_vllm_kv_metrics(base_url)
    t_arm0 = time.perf_counter()

    async def _run_session(sess_turns: List[DeltaTurn]) -> None:
        nonlocal compactions_total
        eid = sess_turns[0].entity_id
        fix = fix_map[eid]
        # Session-local entity state so concurrent sessions mutate independently
        compact_stale = 1 if arm_name == "S-DELTA-COMPACT" else 999
        delta_ent = DeltaFramedEntity(
            entity_id=fix.entity_id,
            name=fix.name,
            base_dossier=fix.base_dossier,
            compact_delta_ratio=0.95,
            compact_max_stale=compact_stale,
            skeleton_max_chars=10000,
        )
        naive_doc = fix.base_dossier
        prev_sys_prompt: Optional[str] = None

        for tr in sess_turns:
            if tr.mutation_before is not None:
                mb = tr.mutation_before
                if arm_name in ("S-DELTA-KV", "S-DELTA-COMPACT", "S-HYBRID-HIER-KV"):
                    res = delta_ent.append_delta(
                        mb.field_name, mb.new_value, note=mb.note, auto_compact=(arm_name == "S-DELTA-COMPACT")
                    )
                    if res["compacted"]:
                        compactions_total += 1
                elif arm_name in ("S-NAIVE-REWRITE", "S-FULL-64K-KV", "S-RAG-LIVE", "S-RAG-64K"):
                    naive_doc = _mutate_in_place(naive_doc, mb.field_name, mb.new_value)

            t_ret0 = time.perf_counter()
            if arm_name in ("S-NAIVE-REWRITE", "S-FULL-64K-KV"):
                sys_content = f"{sys_preamble}\n\n{DELTA_OVERRIDE_RULE}\n\n=== ENTITY DOSSIER: {fix.name} ===\n{naive_doc}"
                user_content = tr.question
                tier = "FULL-KV"
            elif arm_name in ("S-DELTA-KV", "S-DELTA-COMPACT"):
                sys_content = delta_ent.render_full_prefix(sys_preamble)
                user_content = tr.question
                tier = "FULL-KV"
            elif arm_name == "S-HYBRID-HIER-KV":
                decision = policy.classify(delta_ent)
                tier = decision.tier
                sys_content = delta_ent.render_hierarchical_prefix(sys_preamble)
                cold_docs = delta_ent.retrieve_cold_chunks(tr.question, top_k=3)
                ctx = "\n\n---\n\n".join(cold_docs)
                user_content = f"Entity archive excerpts:\n{ctx}\n\nQuestion:\n{tr.question}"
            else:
                # Stateless RAG over live updated chunks (no entity session carry-over)
                tier = "CHUNKED-RAG"
                sys_content = sys_preamble
                if naive_doc != fix.base_dossier:
                    # Fast entity-updated chunk merge: search global index + live entity header chunk
                    docs, _ = global_rag(tr.question)  # type: ignore[misc]
                    live_hdr = _chunk_text(eid, naive_doc)[0][1]
                    # Replace stale header chunk of this entity if retrieved so RAG sees the live write!
                    docs = [live_hdr if (fix.name in d and "SLA Target:" in d) else d for d in docs]
                else:
                    docs, _ = global_rag(tr.question)  # type: ignore[misc]
                ctx = "\n\n---\n\n".join(docs)
                user_content = f"Retrieved context:\n{ctx}\n\nQuestion:\n{tr.question}"

            ret_ms = (time.perf_counter() - t_ret0) * 1000.0
            if prev_sys_prompt is not None and tr.mutation_before is not None:
                preserved_ratios.append(prefix_preserved_ratio(prev_sys_prompt, sys_content))
            prev_sys_prompt = sys_content

            messages = [
                {"role": "system", "content": sys_content},
                {"role": "user", "content": user_content},
            ]
            async with sem:
                resp = await _call_vllm_stream(base_url, model, messages, max_tokens=max_tokens)

            acc = _score_exact(resp["text"], tr.expected) if resp["error"] is None else 0
            turn_logs.append(
                {
                    "arm": arm_name,
                    "qid": tr.qid,
                    "entity_id": tr.entity_id,
                    "turn_idx": tr.turn_idx,
                    "category": tr.category,
                    "had_mutation": tr.mutation_before is not None,
                    "tier": tier,
                    "sys_chars": len(sys_content),
                    "retrieve_ms": round(ret_ms, 2),
                    "q_to_first_s": round(resp["q_to_first_s"] + ret_ms / 1000.0, 4),
                    "q_to_end_s": round(resp["q_to_end_s"] + ret_ms / 1000.0, 4),
                    "expected": tr.expected,
                    "pred": resp["text"][:160],
                    "correct": acc,
                    "error": resp["error"],
                }
            )

    await asyncio.gather(*(_run_session(s) for s in sessions))
    wall_s = time.perf_counter() - t_arm0
    m_after = _scrape_vllm_kv_metrics(base_url)

    n_turns = len(turn_logs)
    errors = sum(1 for r in turn_logs if r["error"] is not None)
    q_first = [r["q_to_first_s"] for r in turn_logs]
    q_end = [r["q_to_end_s"] for r in turn_logs]
    mut_first = [r["q_to_first_s"] for r in turn_logs if r["had_mutation"]]
    acc_all = 100.0 * sum(r["correct"] for r in turn_logs) / max(1, n_turns)
    mut_rows = [r for r in turn_logs if r["category"] == "mutated_fact"]
    acc_mut = 100.0 * sum(r["correct"] for r in mut_rows) / max(1, len(mut_rows))
    deep_rows = [r for r in turn_logs if r["category"] == "deep_clause"]
    acc_deep = 100.0 * sum(r["correct"] for r in deep_rows) / max(1, len(deep_rows)) if deep_rows else None

    summary = {
        "arm": arm_name,
        "turns": n_turns,
        "errors": errors,
        "wall_s": round(wall_s, 3),
        "throughput_tpm": round(n_turns / (wall_s / 60.0), 2),
        "p50_q_first_s": round(float(np.percentile(q_first, 50)), 4),
        "p95_q_first_s": round(float(np.percentile(q_first, 95)), 4),
        "p95_post_mut_first_s": round(float(np.percentile(mut_first, 95)), 4) if mut_first else 0.0,
        "p95_q_end_s": round(float(np.percentile(q_end, 95)), 4),
        "mean_sys_tokens": int(round(float(np.mean([r["sys_chars"] for r in turn_logs])) / CHARS_PER_TOKEN)),
        "mean_retrieve_ms": round(float(np.mean([r["retrieve_ms"] for r in turn_logs])), 2),
        "gpu_kv_hit_pct": _kv_hit_pct(m_before, m_after),
        "prefix_preserved_on_write_pct": round(100.0 * float(np.mean(preserved_ratios)), 2) if preserved_ratios else 0.0,
        "compactions": compactions_total,
        "accuracy_pct": round(acc_all, 2),
        "mutated_acc_pct": round(acc_mut, 2),
        "deep_clause_acc_pct": round(acc_deep, 2) if acc_deep is not None else None,
    }
    return {"summary": summary, "turns": turn_logs}


async def run_all(args: argparse.Namespace) -> None:
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    results: Dict[str, Any] = {"model": args.model, "exp_a_mutating": [], "exp_b_64k_wall": []}

    if not args.skip_a:
        print("=== Sub-Experiment A: Mutating-State Live Writes (~12K/entity) ===", flush=True)
        fix_a, sess_a = build_mutating_workload(
            n_entities=args.a_entities,
            sessions_per_entity=args.a_sessions_per_entity,
            target_tokens=args.a_tokens,
        )
        for arm in ("S-NAIVE-REWRITE", "S-DELTA-KV", "S-DELTA-COMPACT", "S-RAG-LIVE"):
            print(f"Running {arm} ({sum(len(s) for s in sess_a)} turns, n={args.concurrency})...", flush=True)
            res = await run_arm(arm, args.base_url, args.model, fix_a, sess_a, concurrency=args.concurrency)
            results["exp_a_mutating"].append(res["summary"])
            (out_dir / f"exp_a_{arm}.jsonl").write_text(
                "\n".join(json.dumps(r) for r in res["turns"]) + "\n", encoding="utf-8"
            )
            print("  ->", json.dumps(res["summary"]), flush=True)

    if not args.skip_b:
        print("=== Sub-Experiment B: Breaking the 64K KV Capacity Wall (~64K/entity) ===", flush=True)
        fix_b, sess_b = build_64k_wall_workload(
            n_entities=args.b_entities,
            sessions_per_entity=args.b_sessions_per_entity,
            target_tokens=args.b_tokens,
        )
        for arm in ("S-FULL-64K-KV", "S-RAG-64K", "S-HYBRID-HIER-KV"):
            print(f"Running {arm} ({sum(len(s) for s in sess_b)} turns, n={args.b_concurrency})...", flush=True)
            res = await run_arm(arm, args.base_url, args.model, fix_b, sess_b, concurrency=args.b_concurrency)
            results["exp_b_64k_wall"].append(res["summary"])
            (out_dir / f"exp_b_{arm}.jsonl").write_text(
                "\n".join(json.dumps(r) for r in res["turns"]) + "\n", encoding="utf-8"
            )
            print("  ->", json.dumps(res["summary"]), flush=True)

    (out_dir / "summary.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
    print(f"Saved results to {out_dir / 'summary.json'}", flush=True)


def main() -> None:
    p = argparse.ArgumentParser(description="Experiment 3: Delta-Framed Entity KV + Breaking the 64K Wall")
    p.add_argument("--base-url", default="http://127.0.0.1:8000")
    p.add_argument("--model", default="Qwen/Qwen3-8B-FP8")
    p.add_argument("--out-dir", default="/tmp/kv_delta_results")
    p.add_argument("--concurrency", type=int, default=16)
    p.add_argument("--a-entities", type=int, default=16)
    p.add_argument("--a-sessions-per-entity", type=int, default=2)
    p.add_argument("--a-tokens", type=int, default=12000)
    p.add_argument("--b-concurrency", type=int, default=16)
    p.add_argument("--b-entities", type=int, default=8)
    p.add_argument("--b-sessions-per-entity", type=int, default=2)
    p.add_argument("--b-tokens", type=int, default=64000)
    p.add_argument("--skip-a", action="store_true")
    p.add_argument("--skip-b", action="store_true")
    args = p.parse_args()
    asyncio.run(run_all(args))


if __name__ == "__main__":
    main()
