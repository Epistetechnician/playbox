#!/usr/bin/env python3
"""Multi-replica sticky routing benchmark: Round-Robin vs Pure Sticky vs Bounded-Load Sticky.

Tests 3 multi-replica load-balancing policies across K independent vLLM workers
under Uniform and skewed Zipf entity traffic:
  - RR: stateless round-robin (destroys prefix cache locality across K replicas)
  - STICKY-HASH: consistent-hash per-entity pinning (maximizes locality, hot-spots on Zipf)
  - STICKY-BOUNDED: consistent-hash with bounded-load spillover to secondary replica
"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import math
import random
import re
import time
import uuid
from dataclasses import dataclass
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

try:
    import aiohttp
except ImportError:  # pragma: no cover - optional for pure-Python unit tests
    aiohttp = None  # type: ignore[assignment]

_TOOLS_DIR = str(Path(__file__).resolve().parent)
if _TOOLS_DIR not in sys.path:
    sys.path.insert(0, _TOOLS_DIR)

from playbox_steer.kv_sota_bench import SotaEntityRouter
from kv_crossover_run import CrossoverQuestion, build_crossover_corpus


def _pct(vals: Sequence[float], p: float) -> float:
    if not vals:
        return 0.0
    xs = sorted(vals)
    idx = min(len(xs) - 1, max(0, int(math.ceil((p / 100.0) * len(xs))) - 1))
    return float(xs[idx])


def _extract_answer_text(text: str) -> str:
    m = re.search(r"Answer:\s*(.+)", text, flags=re.I)
    return (m.group(1) if m else text).strip()


def _score_exact(pred: str, expected: str) -> float:
    p = _extract_answer_text(pred).lower()
    e = expected.strip().lower()
    if e in p:
        return 1.0
    p_num = re.sub(r"[^0-9a-z.-]", "", p)
    e_num = re.sub(r"[^0-9a-z.-]", "", e)
    return 1.0 if (e_num and e_num in p_num) else 0.0


class ReplicaLoadBalancer:
    """Multi-replica router supporting RR, STICKY-HASH, STICKY-BOUNDED, and STICKY-HOT-REP."""

    def __init__(
        self,
        backends: Sequence[str],
        policy: str,
        cap_factor: float = 1.25,
        min_cap: int = 3,
        hot_replicas: int = 2,
        hot_share_threshold: float = 0.15,
        hot_min_requests: int = 8,
    ) -> None:
        if not backends:
            raise ValueError("backends must be non-empty")
        if policy not in ("RR", "STICKY-HASH", "STICKY-BOUNDED", "STICKY-HOT-REP"):
            raise ValueError(f"Unknown policy: {policy}")
        self.backends = [b.rstrip("/") for b in backends]
        self.policy = policy
        self.cap_factor = float(cap_factor)
        self.min_cap = int(min_cap)
        self.hot_replicas = max(1, min(int(hot_replicas), len(self.backends)))
        self.hot_share_threshold = float(hot_share_threshold)
        self.hot_min_requests = int(hot_min_requests)
        self.inflight: List[int] = [0] * len(self.backends)
        self.routed_counts: List[int] = [0] * len(self.backends)
        self.spills: int = 0
        self._rr_cursor: int = 0
        self._entity_hits: Dict[str, int] = {}
        self._total_hits: int = 0

    def _pref_ring(self, entity_id: str) -> List[int]:
        k = len(self.backends)
        scored = []
        for idx in range(k):
            h = hashlib.sha256(f"{entity_id}::replica::{idx}".encode("utf-8")).digest()
            scored.append((int.from_bytes(h[:8], "big"), idx))
        scored.sort()
        return [idx for _, idx in scored]

    def _is_hot(self, entity_id: str) -> bool:
        if self.policy != "STICKY-HOT-REP" or len(self.backends) < 2 or self.hot_replicas < 2:
            return False
        if self._total_hits < self.hot_min_requests:
            return False
        return (self._entity_hits.get(entity_id, 0) / self._total_hits) >= self.hot_share_threshold

    def acquire(self, entity_id: str) -> Tuple[int, str, bool]:
        """Select a backend replica index, increment inflight, return (idx, url, spilled)."""
        k = len(self.backends)
        self._entity_hits[entity_id] = self._entity_hits.get(entity_id, 0) + 1
        self._total_hits += 1
        spilled = False
        if self.policy == "RR":
            idx = self._rr_cursor % k
            self._rr_cursor += 1
        elif self.policy == "STICKY-HASH":
            idx = self._pref_ring(entity_id)[0]
        elif self.policy == "STICKY-HOT-REP" and self._is_hot(entity_id):
            ring = self._pref_ring(entity_id)
            total_in = sum(self.inflight) + 1
            cap = max(self.min_cap, int(math.ceil(self.cap_factor * total_in / k)))
            warm = ring[: self.hot_replicas]
            warm_best = min(warm, key=lambda c: (self.inflight[c], self.routed_counts[c]))
            if self.inflight[warm_best] < cap:
                idx = warm_best
            else:
                chosen = next((c for c in ring[self.hot_replicas :] if self.inflight[c] < cap), None)
                idx = chosen if chosen is not None else min(ring, key=lambda c: self.inflight[c])
                spilled = idx not in warm
        else:
            ring = self._pref_ring(entity_id)
            total_in = sum(self.inflight) + 1
            cap = max(self.min_cap, int(math.ceil(self.cap_factor * total_in / k)))
            chosen = None
            for pos, cand in enumerate(ring):
                if self.inflight[cand] < cap:
                    chosen = cand
                    spilled = pos > 0
                    break
            if chosen is None:
                chosen = min(ring, key=lambda c: self.inflight[c])
                spilled = chosen != ring[0]
            idx = chosen
        self.inflight[idx] += 1
        self.routed_counts[idx] += 1
        if spilled:
            self.spills += 1
        return idx, self.backends[idx], spilled

    def release(self, idx: int) -> None:
        self.inflight[idx] = max(0, self.inflight[idx] - 1)

    def imbalance_ratio(self) -> float:
        mean_r = sum(self.routed_counts) / max(1, len(self.routed_counts))
        if mean_r <= 0:
            return 1.0
        return round(max(self.routed_counts) / mean_r, 3)


def sample_session_entities(
    entity_ids: Sequence[str],
    n_sessions: int,
    zipf_alpha: float = 0.0,
    seed: int = 42,
) -> List[str]:
    """Generate session entity assignments under Uniform (alpha=0) or Zipf(alpha)."""
    rng = random.Random(seed)
    n = len(entity_ids)
    if zipf_alpha <= 0.0:
        return [entity_ids[i % n] for i in range(n_sessions)]
    weights = [1.0 / ((rank + 1) ** zipf_alpha) for rank in range(n)]
    total = sum(weights)
    probs = [w / total for w in weights]
    return rng.choices(list(entity_ids), weights=probs, k=n_sessions)


async def _fetch_metrics(session: aiohttp.ClientSession, base_url: str) -> Dict[str, float]:
    out: Dict[str, float] = {}
    try:
        async with session.get(f"{base_url}/metrics", timeout=aiohttp.ClientTimeout(total=10)) as resp:
            txt = await resp.text()
    except Exception:
        return out
    for line in txt.splitlines():
        if not line or line.startswith("#"):
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


async def _fetch_cluster_metrics(session: aiohttp.ClientSession, backends: Sequence[str]) -> Dict[str, float]:
    results = await asyncio.gather(*(_fetch_metrics(session, b) for b in backends))
    agg: Dict[str, float] = {}
    for m in results:
        for k, v in m.items():
            agg[k] = agg.get(k, 0.0) + v
    return agg


async def _chat_call(
    session: aiohttp.ClientSession,
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
        "max_tokens": max_tokens,
        "temperature": 0.0,
        "seed": seed,
        "stream": True,
        "stream_options": {"include_usage": True},
    }
    if extra_body:
        payload.update(extra_body)
    t0 = time.perf_counter()
    first_t: Optional[float] = None
    chunks: List[str] = []
    prompt_tokens = 0
    completion_tokens = 0
    err: Optional[str] = None
    try:
        async with session.post(
            f"{base_url}/v1/chat/completions",
            json=payload,
            timeout=aiohttp.ClientTimeout(total=180),
        ) as resp:
            if resp.status != 200:
                err = f"HTTP {resp.status}: {(await resp.text())[:200]}"
            else:
                async for raw in resp.content:
                    line = raw.decode("utf-8", errors="ignore").strip()
                    if not line.startswith("data:"):
                        continue
                    data = line[5:].strip()
                    if data == "[DONE]":
                        break
                    try:
                        obj = json.loads(data)
                    except json.JSONDecodeError:
                        continue
                    usage = obj.get("usage")
                    if isinstance(usage, dict):
                        prompt_tokens = int(usage.get("prompt_tokens") or prompt_tokens)
                        completion_tokens = int(usage.get("completion_tokens") or completion_tokens)
                    choices = obj.get("choices") or []
                    if choices:
                        delta = (choices[0].get("delta") or {}).get("content")
                        if delta:
                            if first_t is None:
                                first_t = time.perf_counter()
                            chunks.append(delta)
    except Exception as exc:
        err = f"{type(exc).__name__}: {exc}"
    t_end = time.perf_counter()
    return {
        "text": "".join(chunks),
        "ttft_s": (first_t - t0) if first_t is not None else (t_end - t0),
        "end_s": t_end - t0,
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "error": err,
    }


async def run_multinode_cell(
    backends: Sequence[str],
    model: str,
    policy: str,
    dist_name: str,
    zipf_alpha: float,
    dossiers: Dict[str, str],
    entity_questions: Dict[str, List[CrossoverQuestion]],
    router: SotaEntityRouter,
    n_sessions: int,
    concurrency: int,
    max_tokens: int,
    cap_factor: float,
    min_cap: int,
    seed: int,
) -> Dict[str, Any]:
    salt = f"[MN-{uuid.uuid4().hex[:8]}]"
    entity_ids = sorted(dossiers.keys())
    session_entities = sample_session_entities(entity_ids, n_sessions=n_sessions, zipf_alpha=zipf_alpha, seed=seed)
    lb = ReplicaLoadBalancer(backends, policy=policy, cap_factor=cap_factor, min_cap=min_cap)
    extra_body = {"chat_template_kwargs": {"enable_thinking": False}}

    connector = aiohttp.TCPConnector(limit=max(64, concurrency * 4))
    async with aiohttp.ClientSession(connector=connector) as http_sess:
        m0 = await _fetch_cluster_metrics(http_sess, backends)
        sem = asyncio.Semaphore(concurrency)
        records: List[Dict[str, Any]] = []
        t_wall0 = time.perf_counter()

        async def _run_session(sess_idx: int, eid: str) -> None:
            qs = entity_questions[eid]
            async with sem:
                active_entity: Optional[str] = None
                for turn_idx, q in enumerate(qs):
                    t_q0 = time.perf_counter()
                    matched, rmode = router.resolve(q.question, active_entity=active_entity)
                    if rmode == "entity_hit" and (active_entity is None or matched == active_entity):
                        active_entity = matched
                    target_entity = active_entity or eid
                    dossier = dossiers[target_entity]
                    messages = [
                        {
                            "role": "system",
                            "content": (
                                f"{salt} You are an enterprise account assistant. "
                                f"Answer concisely with the exact value from the dossier. "
                                f"End with 'Answer: <value>'.\n\n{dossier}"
                            ),
                        },
                        {"role": "user", "content": q.question},
                    ]
                    rep_idx, url, spilled = lb.acquire(target_entity)
                    try:
                        res = await _chat_call(http_sess, url, model, messages, max_tokens, seed + turn_idx, extra_body)
                    finally:
                        lb.release(rep_idx)
                    t_q_end = time.perf_counter()
                    pre_s = max(0.0, (t_q_end - t_q0) - res["end_s"])
                    q_first_s = pre_s + res["ttft_s"]
                    q_end_s = t_q_end - t_q0
                    score = 0.0 if res["error"] else _score_exact(res["text"], q.answer)
                    records.append(
                        {
                            "session": sess_idx,
                            "turn": turn_idx + 1,
                            "entity_id": target_entity,
                            "replica": rep_idx,
                            "spilled": spilled,
                            "q_first_s": round(q_first_s, 4),
                            "q_end_s": round(q_end_s, 4),
                            "prompt_tokens": res["prompt_tokens"],
                            "completion_tokens": res["completion_tokens"],
                            "score": score,
                            "error": res["error"],
                        }
                    )

        await asyncio.gather(*(_run_session(i, eid) for i, eid in enumerate(session_entities)))
        wall_s = time.perf_counter() - t_wall0
        m1 = await _fetch_cluster_metrics(http_sess, backends)

    dh = max(0.0, m1.get("vllm:prefix_cache_hits_total", 0.0) - m0.get("vllm:prefix_cache_hits_total", 0.0))
    dq = max(0.0, m1.get("vllm:prefix_cache_queries_total", 0.0) - m0.get("vllm:prefix_cache_queries_total", 0.0))
    gpu_hit_pct = (100.0 * dh / dq) if dq > 0 else 0.0

    q1 = [r["q_first_s"] for r in records]
    t2 = [r["q_first_s"] for r in records if r["turn"] >= 2]
    qe = [r["q_end_s"] for r in records]
    errs = sum(1 for r in records if r["error"])
    acc = sum(r["score"] for r in records) / max(1, len(records))
    tpm = (len(records) / wall_s) * 60.0 if wall_s > 0 else 0.0

    summary = {
        "distribution": dist_name,
        "zipf_alpha": zipf_alpha,
        "policy": policy,
        "n_replicas": len(backends),
        "n_sessions": n_sessions,
        "n_turns": len(records),
        "concurrency": concurrency,
        "wall_s": round(wall_s, 2),
        "turns_per_min": round(tpm, 2),
        "p50_q_first_s": round(_pct(q1, 50), 3),
        "p95_q_first_s": round(_pct(q1, 95), 3),
        "p95_t2_first_s": round(_pct(t2, 95), 3),
        "p95_q_end_s": round(_pct(qe, 95), 3),
        "gpu_prefix_hit_pct": round(gpu_hit_pct, 2),
        "accuracy": round(acc, 4),
        "errors": errs,
        "replica_counts": list(lb.routed_counts),
        "imbalance_ratio": lb.imbalance_ratio(),
        "spill_pct": round(100.0 * lb.spills / max(1, len(records)), 2),
    }
    return {"summary": summary, "records": records}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--backends", required=True, help="Comma-separated vLLM URLs, e.g. http://127.0.0.1:8001,...")
    ap.add_argument("--model", default="Qwen/Qwen3-8B-FP8")
    ap.add_argument("--policies", default="RR,STICKY-HASH,STICKY-BOUNDED")
    ap.add_argument("--dists", default="uniform:0.0,zipf12:1.2")
    ap.add_argument("--target-tokens", type=int, default=4096)
    ap.add_argument("--n-entities", type=int, default=48)
    ap.add_argument("--n-sessions", type=int, default=48)
    ap.add_argument("--turns-per-session", type=int, default=6)
    ap.add_argument("--concurrency", type=int, default=24)
    ap.add_argument("--max-tokens", type=int, default=64)
    ap.add_argument("--cap-factor", type=float, default=1.25)
    ap.add_argument("--min-cap", type=int, default=3)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    backends = [b.strip() for b in args.backends.split(",") if b.strip()]
    policies = [p.strip() for p in args.policies.split(",") if p.strip()]
    dists: List[Tuple[str, float]] = []
    for item in args.dists.split(","):
        name, alpha_s = item.split(":")
        dists.append((name.strip(), float(alpha_s)))

    corpus = build_crossover_corpus(
        args.target_tokens,
        n_entities=args.n_entities,
        turns_per_session=args.turns_per_session,
        seed=args.seed,
    )
    aliases = {
        f"ACME-{100 + i:03d}": [f"ACME-{100 + i:03d}", f"Meridian Systems {i + 1:02d}"]
        for i in range(args.n_entities)
    }
    router = SotaEntityRouter(corpus.dossiers, aliases)
    entity_questions: Dict[str, List[CrossoverQuestion]] = {}
    for sess in corpus.sessions:
        for q in sess:
            entity_questions.setdefault(q.entity_id, []).append(q)

    all_summaries: List[Dict[str, Any]] = []
    for dist_name, alpha in dists:
        for policy in policies:
            print(
                f"Running dist={dist_name} (alpha={alpha}) policy={policy} "
                f"replicas={len(backends)} n={args.concurrency} ...",
                flush=True,
            )
            res = asyncio.run(
                run_multinode_cell(
                    backends=backends,
                    model=args.model,
                    policy=policy,
                    dist_name=dist_name,
                    zipf_alpha=alpha,
                    dossiers=corpus.dossiers,
                    entity_questions=entity_questions,
                    router=router,
                    n_sessions=args.n_sessions,
                    concurrency=args.concurrency,
                    max_tokens=args.max_tokens,
                    cap_factor=args.cap_factor,
                    min_cap=args.min_cap,
                    seed=args.seed,
                )
            )
            s = res["summary"]
            all_summaries.append(s)
            (out_dir / f"{dist_name}_{policy}.json").write_text(json.dumps(res, indent=2))
            (out_dir / "summary.json").write_text(json.dumps(all_summaries, indent=2))
            print(
                f"  -> {dist_name} {policy}: {s['turns_per_min']} tpm | "
                f"p95_q1={s['p95_q_first_s']}s t2={s['p95_t2_first_s']}s end={s['p95_q_end_s']}s | "
                f"gpu_hit={s['gpu_prefix_hit_pct']}% | imb={s['imbalance_ratio']}x spill={s['spill_pct']}% | "
                f"counts={s['replica_counts']} | acc={s['accuracy']*100:.1f}% err={s['errors']}",
                flush=True,
            )


if __name__ == "__main__":
    main()
