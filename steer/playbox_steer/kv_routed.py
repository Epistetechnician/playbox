"""Three-arm benchmark: Inline RAG (S-RAG) vs Shared 128K KV (S-CTX) vs Entity-Routed Working-Set KV + Adaptive RAG (S-ROUTED).

Designed to run apples-to-apples on the 128K CRM corpus (corpus_128k.jsonl + sessions_v2.jsonl) with:
- Zero-oracle lexical longest-match entity routing (handles look-alike account names cleanly)
- Per-account working-set KV prefix (~4-6K tokens: primary account docs + linked tickets/emails + shared policy/product docs)
- Adaptive fallback in the user turn on cross-account drift (e.g. Cat B comparisons) or unrouted queries, preserving the cached prefix
- Question-arrival end-to-end timing (question_to_first_ms, question_to_end_ms)
- Fresh per-cell UUID prefix salt and counterbalanced arm ordering across sweeps
- Pooled and per-sweep p95s, bootstrap 95% CIs, and paired exact McNemar tests
"""
from __future__ import annotations

import copy
import difflib
import json
import math
import random
import re
import threading
import time
import urllib.request
import uuid
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, List, Optional, Sequence, Tuple

WORDS = re.compile(r"[a-z0-9]+")
_PRONOUN_RE = re.compile(
    r"\b(this account|that account|the account|this customer|that customer|their|they|its)\b",
    re.IGNORECASE,
)
_POLICY_OR_CROSS_RE = re.compile(
    r"\b(policy|policies|sla|threshold|discount approval|incident updates|soc 2|forecast|health score|compare|between|which account|which customer|both accounts)\b",
    re.IGNORECASE,
)

SYSTEM_BASE = "You are an enterprise assistant. Answer questions using only the context provided."

ANSWER_FORMATS = {
    "exact": "Answer: <the value only>",
    "set": "Answer: <the account name, or the account names separated by commas>",
    "count": "Answer: <the number only>",
    "rubric": "Answer: <each requested item with its value, separated by semicolons>",
}


def format_user_question(q: Dict[str, Any]) -> str:
    fmt = ANSWER_FORMATS.get(q.get("answer_type", "exact"), ANSWER_FORMATS["exact"])
    return (
        f"{q['text']}\n\nUse only the context. First state the facts you rely on in one "
        f"or two short sentences. Then finish with one final line in this form: {fmt}\n"
        "If the context does not contain the answer, that line is: "
        "Answer: No such record exists."
    )


def system_with_context(ctx: str, salt: str) -> str:
    body = ctx if ctx else "(none)"
    return f"Workspace {salt}\n{SYSTEM_BASE}\n\n# Context\n\n{body}"


class SimpleBM25:
    """Lightweight stdlib BM25 retriever used for local tests and fallback retrieval."""

    def __init__(self, docs: Sequence[Dict[str, Any]], text_key: str = "text", id_key: str = "chunk_id"):
        self.docs = list(docs)
        self.id_key = id_key
        self.toks = [WORDS.findall(d[text_key].lower()) for d in self.docs]
        self.avg = (sum(len(t) for t in self.toks) / len(self.toks)) if self.toks else 1.0
        self.tf = [Counter(t) for t in self.toks]
        df = Counter(w for t in self.toks for w in set(t))
        n = len(self.docs)
        self.idf = {w: math.log(1 + (n - c + 0.5) / (c + 0.5)) for w, c in df.items()}

    def search(self, query: str, k: int = 5) -> Tuple[List[str], Dict[str, float]]:
        t0 = time.perf_counter()
        q = WORDS.findall(query.lower())
        scores: List[Tuple[float, int]] = []
        for i, tf in enumerate(self.tf):
            L = len(self.toks[i])
            s = 0.0
            for w in q:
                f = tf.get(w, 0)
                if f:
                    s += self.idf.get(w, 0.0) * f * 2.5 / (f + 1.5 * (0.25 + 0.75 * L / self.avg))
            scores.append((s, i))
        scores.sort(key=lambda x: -x[0])
        ids = [self.docs[i][self.id_key] for _, i in scores[:k]]
        ms = (time.perf_counter() - t0) * 1000.0
        return ids, {"search_queue_ms": 0.0, "retrieve_ms": ms, "rerank_ms": 0.0}


class EntityRouter:
    """Zero-oracle longest-match entity router + per-account working-set builder.

    Matches account names in the user question longest-first so look-alike names
    (e.g. 'Northwind Logistics' vs 'Northwind Logistics Group') never collide.
    """

    def __init__(self, docs: Sequence[Dict[str, Any]], entities: Sequence[Dict[str, Any]]):
        self.docs_by_id: Dict[str, Dict[str, Any]] = {d["doc_id"]: d for d in docs}
        self.all_docs: List[Dict[str, Any]] = list(docs)

        # Build longest-first regex patterns for every known CRM entity.
        entries: List[Tuple[str, str, str]] = []
        for e in entities:
            name = e["name"].strip()
            doc_id = e["doc_id"]
            doc = self.docs_by_id.get(doc_id)
            cid = (doc.get("customer_id") if doc else None) or doc_id
            entries.append((name, doc_id, cid))
        entries.sort(key=lambda x: -len(x[0]))
        self._patterns: List[Tuple[re.Pattern[str], str, str, str]] = [
            (re.compile(r"(?<!\w)" + re.escape(name) + r"(?!\w)", re.IGNORECASE), name, doc_id, cid)
            for name, doc_id, cid in entries
        ]

        # Precompute shared operational docs (policy + product) and per-entity linked docs (tickets/emails).
        self.shared_ops_docs: List[Dict[str, Any]] = [
            d for d in docs if d.get("kind") in ("policy", "product")
        ]
        self._entity_dossiers: Dict[str, List[Dict[str, Any]]] = {}
        self._entity_names: Dict[str, str] = {doc_id: name for name, doc_id, _ in entries}
        ops_linked = [d for d in docs if d.get("kind") in ("tickets", "email")]
        for name, doc_id, cid in entries:
            linked: List[Dict[str, Any]] = []
            if doc_id in self.docs_by_id:
                linked.append(self.docs_by_id[doc_id])
            pat_name = re.compile(r"(?<!\w)" + re.escape(name) + r"(?!\w)", re.IGNORECASE)
            pat_cid = re.compile(r"(?<!\w)" + re.escape(cid) + r"(?!\w)", re.IGNORECASE) if cid else None
            for d in ops_linked:
                if d.get("customer_id") == cid or pat_name.search(d["text"]) or (pat_cid and pat_cid.search(d["doc_id"])):
                    linked.append(d)
            self._entity_dossiers[doc_id] = linked

    def detect_entities(self, question: str, fuzzy: bool = True) -> List[str]:
        """Return ordered unique doc_ids matched in question without overlapping spans.

        Runs exact longest-match first; if none match and fuzzy=True, tries sliding-window
        edit-distance matching (for typos/dropped suffixes) only when the best match clears
        a strict margin over the runner-up so look-alike accounts never collide.
        """
        hits: List[Tuple[int, int, str]] = []
        occupied: List[Tuple[int, int]] = []
        for pat, _name, doc_id, _cid in self._patterns:
            for m in pat.finditer(question):
                s, e = m.span()
                if any(not (e <= os_ or s >= oe_) for os_, oe_ in occupied):
                    continue
                occupied.append((s, e))
                hits.append((s, e, doc_id))
        hits.sort(key=lambda x: x[0])
        out: List[str] = []
        for _, _, doc_id in hits:
            if doc_id not in out:
                out.append(doc_id)
        if out or not fuzzy:
            return out

        q_toks = WORDS.findall(question.lower())
        if not q_toks:
            return []
        scored: List[Tuple[float, str]] = []
        for _pat, name, doc_id, _cid in self._patterns:
            n_toks = WORDS.findall(name.lower())
            if not n_toks:
                continue
            k = len(n_toks)
            name_norm = " ".join(n_toks)
            best = 0.0
            for win in (k, max(1, k - 1)):
                if win > len(q_toks):
                    continue
                for i in range(len(q_toks) - win + 1):
                    cand = " ".join(q_toks[i : i + win])
                    r = difflib.SequenceMatcher(None, name_norm, cand).ratio()
                    if r > best:
                        best = r
            if best >= 0.84:
                scored.append((best, doc_id))
        if not scored:
            return []
        scored.sort(key=lambda x: -x[0])
        if len(scored) == 1 or (scored[0][0] - scored[1][0]) >= 0.04:
            return [scored[0][1]]
        return []

    def working_set_text(self, primary_doc_id: Optional[str]) -> str:
        """Build the stable ~4-6K token working-set prefix for one routed account."""
        parts: List[Dict[str, Any]] = []
        seen = set()
        if primary_doc_id and primary_doc_id in self._entity_dossiers:
            for d in self._entity_dossiers[primary_doc_id]:
                if d["doc_id"] not in seen:
                    seen.add(d["doc_id"])
                    parts.append(d)
        for d in self.shared_ops_docs:
            if d["doc_id"] not in seen:
                seen.add(d["doc_id"])
                parts.append(d)
        return "\n\n".join(d["text"] for d in parts)

    def extra_dossier_text(self, doc_ids: Sequence[str], exclude_primary: Optional[str]) -> str:
        """Return dossiers for any secondary accounts referenced on a drift/comparison turn."""
        parts: List[str] = []
        seen = set()
        for did in doc_ids:
            if did == exclude_primary or did not in self._entity_dossiers:
                continue
            for d in self._entity_dossiers[did]:
                if d["doc_id"] not in seen:
                    seen.add(d["doc_id"])
                    parts.append(d["text"])
        return "\n\n".join(parts)


@dataclass
class CorpusBundle:
    docs: List[Dict[str, Any]]
    chunks: Dict[str, Dict[str, Any]]
    entities: List[Dict[str, Any]]
    queries: Dict[str, Dict[str, Any]]
    sessions: List[Dict[str, Any]]
    corpus_text: str = field(init=False)

    def __post_init__(self) -> None:
        self.corpus_text = "\n\n".join(d["text"] for d in self.docs)


def _inject_typo(name: str, rng: random.Random) -> str:
    """Introduce a realistic single-character deletion or adjacent swap in the longest word."""
    parts = name.split(" ")
    longest_i = max(range(len(parts)), key=lambda i: len(parts[i]))
    w = parts[longest_i]
    if len(w) >= 5:
        pos = rng.randrange(1, len(w) - 2)
        if rng.random() < 0.5:
            w = w[:pos] + w[pos + 1 :]
        else:
            w = w[:pos] + w[pos + 1] + w[pos] + w[pos + 2 :]
        parts[longest_i] = w
    return " ".join(parts)


def corrupt_bundle_noisy(
    bundle: CorpusBundle,
    router: EntityRouter,
    typo_turn1_frac: float = 0.35,
    pronoun_followup_frac: float = 0.50,
    seed: int = 42,
) -> Tuple[CorpusBundle, Dict[str, int]]:
    """Create a noisy-routing copy of bundle with Turn-1 typos and Turn 2-6 pronoun follow-ups.

    Both S-RAG and S-ROUTED receive the exact same corrupted questions; gold answers are preserved.
    """
    rng = random.Random(seed)
    new_queries = copy.deepcopy(bundle.queries)
    new_sessions = copy.deepcopy(bundle.sessions)
    stats = {"typo_turn1": 0, "pronoun_followup": 0, "unchanged": 0}
    seen_qids = set()

    for s in new_sessions:
        primary_name: Optional[str] = None
        for turn_idx, qid in enumerate(s["turns"], start=1):
            if qid in seen_qids:
                continue
            seen_qids.add(qid)
            q = new_queries[qid]
            matched = router.detect_entities(q["text"], fuzzy=False)
            if turn_idx == 1 and matched:
                primary_name = router._entity_names.get(matched[0])
                if primary_name and rng.random() < typo_turn1_frac:
                    typo = _inject_typo(primary_name, rng)
                    if typo != primary_name:
                        q["text"] = re.sub(
                            re.escape(primary_name), typo, q["text"], count=1, flags=re.IGNORECASE
                        )
                        q["noise_type"] = "typo_turn1"
                        stats["typo_turn1"] += 1
                        continue
            elif (
                turn_idx >= 2
                and q.get("category") == "A"
                and len(matched) == 1
                and primary_name
                and router._entity_names.get(matched[0]) == primary_name
                and rng.random() < pronoun_followup_frac
            ):
                replaced = re.sub(
                    re.escape(primary_name), "this account", q["text"], count=1, flags=re.IGNORECASE
                )
                if replaced != q["text"]:
                    q["text"] = replaced
                    q["noise_type"] = "pronoun_followup"
                    stats["pronoun_followup"] += 1
                    continue
            stats["unchanged"] += 1

    return (
        CorpusBundle(
            docs=bundle.docs,
            chunks=bundle.chunks,
            entities=bundle.entities,
            queries=new_queries,
            sessions=new_sessions,
        ),
        stats,
    )


def expand_bundle_tenants(bundle: CorpusBundle, scale: int) -> CorpusBundle:
    """Replicate sessions across `scale` isolated tenants (unique KV prefix per tenant)."""
    if scale <= 1:
        return bundle
    expanded: List[Dict[str, Any]] = []
    for t_idx in range(scale):
        for s in bundle.sessions:
            sc = copy.deepcopy(s)
            sc["session_id"] = f"t{t_idx}_{s['session_id']}"
            sc["tenant_tag"] = f"tenant_{t_idx}"
            expanded.append(sc)
    return CorpusBundle(
        docs=bundle.docs,
        chunks=bundle.chunks,
        entities=bundle.entities,
        queries=bundle.queries,
        sessions=expanded,
    )


def load_crm_bundle(root: Path, max_sessions: Optional[int] = None) -> CorpusBundle:
    docs = [json.loads(l) for l in open(root / "corpus/v2/corpus_128k.jsonl")]
    chunks_list = [json.loads(l) for l in open(root / "corpus/v2/chunks_128k.jsonl")]
    chunks = {c["chunk_id"]: c for c in chunks_list}
    entities = json.loads((root / "queries/entities_v2.json").read_text())
    queries: Dict[str, Dict[str, Any]] = {}
    for p in sorted((root / "queries").glob("queries*.jsonl")):
        for l in open(p):
            q = json.loads(l)
            queries[q["query_id"]] = q
    sessions = [json.loads(l) for l in open(root / "queries/sessions_v2.jsonl")]
    if max_sessions is not None:
        sessions = sessions[:max_sessions]
    return CorpusBundle(docs=docs, chunks=chunks, entities=entities, queries=queries, sessions=sessions)


def stream_chat_completion(
    base_url: str,
    model: str,
    messages: List[Dict[str, str]],
    max_tokens: int = 384,
    temperature: float = 0.0,
    seed: int = 0,
    timeout: float = 300.0,
) -> Dict[str, Any]:
    """Stream an OpenAI-compatible chat completion and measure client TTFT/total + token usage."""
    payload = json.dumps(
        {
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "seed": seed,
            "max_tokens": max_tokens,
            "stream": True,
            "stream_options": {"include_usage": True},
        }
    ).encode("utf-8")
    req = urllib.request.Request(
        f"{base_url.rstrip('/')}/v1/chat/completions",
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    t0 = time.perf_counter()
    first_t: Optional[float] = None
    chunks_out: List[str] = []
    usage: Dict[str, Any] = {}
    finish_reason: Optional[str] = None
    error: Optional[str] = None
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            for raw in resp:
                line = raw.decode("utf-8", errors="replace").strip()
                if not line.startswith("data:"):
                    continue
                data = line[5:].strip()
                if data == "[DONE]":
                    break
                ev = json.loads(data)
                if ev.get("usage"):
                    usage = ev["usage"]
                choices = ev.get("choices") or []
                if choices:
                    delta = choices[0].get("delta") or {}
                    content = delta.get("content")
                    if content:
                        if first_t is None:
                            first_t = time.perf_counter()
                        chunks_out.append(content)
                    if choices[0].get("finish_reason"):
                        finish_reason = choices[0]["finish_reason"]
    except Exception as exc:  # noqa: BLE001
        error = f"{type(exc).__name__}: {exc}"
    t1 = time.perf_counter()
    prompt_details = usage.get("prompt_tokens_details") or {}
    return {
        "output": "".join(chunks_out),
        "ttft_ms": ((first_t - t0) * 1000.0) if first_t is not None else ((t1 - t0) * 1000.0),
        "total_ms": (t1 - t0) * 1000.0,
        "prompt_tokens": int(usage.get("prompt_tokens") or 0),
        "completion_tokens": int(usage.get("completion_tokens") or 0),
        "cached_tokens": int(prompt_details.get("cached_tokens") or 0),
        "finish_reason": finish_reason,
        "error": error,
    }


def scrape_vllm_metrics(base_url: str, timeout: float = 5.0) -> Dict[str, float]:
    """Read vLLM Prometheus counters for GPU prefix cache and LMCache external hits."""
    wanted = (
        "vllm:prefix_cache_hits_total",
        "vllm:prefix_cache_queries_total",
        "vllm:external_prefix_cache_hits_total",
        "vllm:external_prefix_cache_queries_total",
    )
    out: Dict[str, float] = {}
    try:
        with urllib.request.urlopen(f"{base_url.rstrip('/')}/metrics", timeout=timeout) as resp:
            for line in resp.read().decode("utf-8", errors="replace").splitlines():
                if not line or line.startswith("#"):
                    continue
                for k in wanted:
                    if line.startswith(k):
                        parts = line.split()
                        if len(parts) >= 2:
                            out[k] = out.get(k, 0.0) + float(parts[-1])
    except Exception as exc:  # noqa: BLE001
        out["_scrape_error"] = str(exc)
    return out


def run_cell(
    bundle: CorpusBundle,
    strategy: str,
    concurrency: int,
    block: int,
    base_url: str,
    model: str,
    retriever: Callable[[str], Tuple[List[str], Dict[str, float]]],
    scorer: Optional[Callable[[Dict[str, Any], str], Optional[float]]] = None,
    max_tokens: int = 384,
    router: Optional[EntityRouter] = None,
    wave_eviction: bool = False,
) -> Dict[str, Any]:
    """Run one (strategy, concurrency, block) cell with UUID prefix salting and synchronized start.

    When wave_eviction=True, sessions execute turn-by-turn in waves (all sessions complete turn 1
    before turn 2 begins) so multi-tenant KV working sets exceed GPU HBM and exercise CPU offloading.
    """
    if strategy not in ("S-RAG", "S-CTX", "S-ROUTED"):
        raise ValueError(f"Unknown strategy: {strategy}")
    router = router or EntityRouter(bundle.docs, bundle.entities)
    salt = uuid.uuid4().hex

    # Prime shared workspace prefix for S-CTX (mirrors run_inline.py); S-ROUTED primes per-session on turn 1.
    probe_info: Dict[str, Any] = {"salt": salt, "wave_eviction": wave_eviction}
    if strategy == "S-CTX":
        sys_ctx = system_with_context(bundle.corpus_text, salt)
        probe_msgs = [{"role": "system", "content": sys_ctx}, {"role": "user", "content": "Reply with OK."}]
        p1 = stream_chat_completion(base_url, model, probe_msgs, max_tokens=2)
        p2 = stream_chat_completion(base_url, model, probe_msgs, max_tokens=2)
        probe_info["probe1"] = {k: p1[k] for k in ("ttft_ms", "prompt_tokens", "cached_tokens", "error")}
        probe_info["probe2"] = {k: p2[k] for k in ("ttft_ms", "prompt_tokens", "cached_tokens", "error")}
        if p1["error"] or p2["error"]:
            raise RuntimeError(f"S-CTX priming failed: {p1['error'] or p2['error']}")
    elif strategy == "S-RAG":
        sys_rag = f"Workspace {salt}\n{SYSTEM_BASE}"
        probe_msgs = [{"role": "system", "content": sys_rag}, {"role": "user", "content": "Reply with OK."}]
        p1 = stream_chat_completion(base_url, model, probe_msgs, max_tokens=2)
        probe_info["probe1"] = {k: p1[k] for k in ("ttft_ms", "prompt_tokens", "cached_tokens", "error")}
        if p1["error"]:
            raise RuntimeError(f"S-RAG priming failed: {p1['error']}")

    before_metrics = scrape_vllm_metrics(base_url)
    sessions = bundle.sessions
    turns_out: List[Dict[str, Any]] = []
    lock = threading.Lock()

    def _run_turn(
        script: Dict[str, Any],
        turn_idx: int,
        qid: str,
        history: List[Dict[str, str]],
        primary_doc_id: Optional[str],
    ) -> Tuple[Dict[str, Any], Optional[str]]:
        q = bundle.queries[qid]
        q_start = time.perf_counter()
        stage: Dict[str, float] = {}
        top5: Optional[List[str]] = None
        routed_entities: Optional[List[str]] = None
        used_fallback = False
        route_mode = "none"
        tenant_tag = script.get("tenant_tag")
        session_salt = f"{salt}:{tenant_tag}" if tenant_tag else salt

        if strategy == "S-RAG":
            ids, stage = retriever(q["text"])
            top5 = ids
            ctx_text = "\n\n".join(bundle.chunks[i]["text"] for i in ids if i in bundle.chunks)
            user_msg = f"# Retrieved Context\n\n{ctx_text}\n\n{format_user_question(q)}"
            history.append({"role": "user", "content": user_msg})
        elif strategy == "S-CTX":
            history.append({"role": "user", "content": format_user_question(q)})
        else:  # S-ROUTED
            t_r0 = time.perf_counter()
            matched = router.detect_entities(q["text"])
            routed_entities = matched
            if turn_idx == 1:
                primary_doc_id = matched[0] if matched else None
                ws_text = router.working_set_text(primary_doc_id)
                history.append({"role": "system", "content": system_with_context(ws_text, session_salt)})
            stage["route_ms"] = (time.perf_counter() - t_r0) * 1000.0

            extra_ctx = ""
            if not matched:
                if (
                    turn_idx >= 2
                    and primary_doc_id is not None
                    and _PRONOUN_RE.search(q["text"])
                    and not _POLICY_OR_CROSS_RE.search(q["text"])
                ):
                    route_mode = "pronoun_carry"
                else:
                    route_mode = "rag_fallback"
                    ids, r_stage = retriever(q["text"])
                    stage.update(r_stage)
                    top5 = ids
                    used_fallback = True
                    extra_ctx = "\n\n".join(bundle.chunks[i]["text"] for i in ids if i in bundle.chunks)
            elif any(m != primary_doc_id for m in matched):
                route_mode = "cross_account_dossier"
                used_fallback = True
                extra_ctx = router.extra_dossier_text(matched, exclude_primary=primary_doc_id)
            else:
                route_mode = "entity_hit"

            if extra_ctx:
                user_msg = f"# Additional Context\n\n{extra_ctx}\n\n{format_user_question(q)}"
            else:
                user_msg = format_user_question(q)
            history.append({"role": "user", "content": user_msg})

        pre_dispatch_ms = (time.perf_counter() - q_start) * 1000.0
        res = stream_chat_completion(base_url, model, history, max_tokens=max_tokens)
        q_end_ms = (time.perf_counter() - q_start) * 1000.0
        q_first_ms = pre_dispatch_ms + res["ttft_ms"]
        history.append({"role": "assistant", "content": res["output"]})

        sc = scorer(q, res["output"]) if scorer is not None else None
        row = {
            "strategy": strategy,
            "agents": concurrency,
            "block": block,
            "session": script["session_id"],
            "turn": turn_idx,
            "query": qid,
            "category": q.get("category"),
            "noise_type": q.get("noise_type"),
            "route_mode": route_mode,
            "question_to_first_ms": q_first_ms,
            "question_to_end_ms": q_end_ms,
            "pre_dispatch_ms": pre_dispatch_ms,
            "ttft_ms": res["ttft_ms"],
            "total_ms": res["total_ms"],
            "prompt_tokens": res["prompt_tokens"],
            "cached_tokens": res["cached_tokens"],
            "completion_tokens": res["completion_tokens"],
            "finish_reason": res["finish_reason"],
            "error": res["error"],
            "score": sc,
            "output": res["output"],
            "top5": top5,
            "routed_entities": routed_entities,
            "primary_doc_id": primary_doc_id,
            "used_fallback": used_fallback,
            "stage_ms": stage,
        }
        return row, primary_doc_id

    if not wave_eviction:
        gate = threading.Barrier(min(concurrency, len(sessions)) + 1)

        def run_session(idx_and_script: Tuple[int, Dict[str, Any]]) -> None:
            pos, script = idx_and_script
            if pos < min(concurrency, len(sessions)):
                gate.wait()
            history: List[Dict[str, str]] = []
            primary_doc_id: Optional[str] = None
            tenant_tag = script.get("tenant_tag")
            session_salt = f"{salt}:{tenant_tag}" if tenant_tag else salt
            if strategy == "S-CTX":
                history.append({"role": "system", "content": system_with_context(bundle.corpus_text, session_salt)})
            elif strategy == "S-RAG":
                history.append({"role": "system", "content": f"Workspace {session_salt}\n{SYSTEM_BASE}"})
            local_rows: List[Dict[str, Any]] = []
            for turn_idx, qid in enumerate(script["turns"], start=1):
                row, primary_doc_id = _run_turn(script, turn_idx, qid, history, primary_doc_id)
                local_rows.append(row)
            with lock:
                turns_out.extend(local_rows)

        with ThreadPoolExecutor(max_workers=concurrency) as pool:
            futs = [pool.submit(run_session, item) for item in enumerate(sessions)]
            wall_t0 = time.perf_counter()
            gate.wait()
            for f in futs:
                f.result()
        wall_s = time.perf_counter() - wall_t0
    else:
        # Turn-by-turn wave scheduling: all sessions run turn 1 before any runs turn 2.
        states: List[Dict[str, Any]] = []
        for script in sessions:
            history: List[Dict[str, str]] = []
            tenant_tag = script.get("tenant_tag")
            session_salt = f"{salt}:{tenant_tag}" if tenant_tag else salt
            if strategy == "S-CTX":
                history.append({"role": "system", "content": system_with_context(bundle.corpus_text, session_salt)})
            elif strategy == "S-RAG":
                history.append({"role": "system", "content": f"Workspace {session_salt}\n{SYSTEM_BASE}"})
            states.append({"script": script, "history": history, "primary_doc_id": None})

        max_turns = max((len(s["turns"]) for s in sessions), default=0)
        wall_t0 = time.perf_counter()
        with ThreadPoolExecutor(max_workers=concurrency) as pool:
            for turn_idx in range(1, max_turns + 1):
                wave_items = [st for st in states if len(st["script"]["turns"]) >= turn_idx]

                def _exec_wave(st: Dict[str, Any], ti: int = turn_idx) -> Dict[str, Any]:
                    qid = st["script"]["turns"][ti - 1]
                    row, pdoc = _run_turn(st["script"], ti, qid, st["history"], st["primary_doc_id"])
                    st["primary_doc_id"] = pdoc
                    return row

                futs = [pool.submit(_exec_wave, st) for st in wave_items]
                for f in futs:
                    turns_out.append(f.result())
        wall_s = time.perf_counter() - wall_t0

    after_metrics = scrape_vllm_metrics(base_url)
    delta_metrics = {
        k: after_metrics.get(k, 0.0) - before_metrics.get(k, 0.0)
        for k in set(before_metrics) | set(after_metrics)
    }
    return {
        "strategy": strategy,
        "agents": concurrency,
        "block": block,
        "wall_s": wall_s,
        "turns_per_min": len(turns_out) / (wall_s / 60.0) if wall_s > 0 else 0.0,
        "probe": probe_info,
        "metrics_delta": delta_metrics,
        "turns": turns_out,
    }


def percentile_nearest(values: Sequence[float], p: float) -> float:
    if not values:
        return 0.0
    s = sorted(values)
    idx = round((p / 100.0) * (len(s) - 1))
    return float(s[max(0, min(len(s) - 1, idx))])


def bootstrap_ci(
    values: Sequence[float],
    stat_fn: Callable[[Sequence[float]], float],
    n_boot: int = 1000,
    seed: int = 0,
) -> Tuple[float, float]:
    if not values:
        return (0.0, 0.0)
    rng = random.Random(seed)
    n = len(values)
    samples = [stat_fn([values[rng.randrange(n)] for _ in range(n)]) for _ in range(n_boot)]
    samples.sort()
    return (samples[int(0.025 * n_boot)], samples[int(0.975 * n_boot)])


def mcnemar_exact(scores_a: Dict[Any, float], scores_b: Dict[Any, float]) -> Dict[str, Any]:
    """Exact two-sided binomial McNemar test on binary (== 1.0) paired outcomes."""
    keys = sorted(set(scores_a) & set(scores_b))
    a_only = sum(1 for k in keys if scores_a[k] >= 1.0 and scores_b[k] < 1.0)
    b_only = sum(1 for k in keys if scores_a[k] < 1.0 and scores_b[k] >= 1.0)
    n_disc = a_only + b_only
    if n_disc == 0:
        p = 1.0
    else:
        lo = min(a_only, b_only)
        p = min(1.0, sum(math.comb(n_disc, k) for k in range(0, lo + 1)) * 2.0 / (2 ** n_disc))
    return {"n_paired": len(keys), "a_only": a_only, "b_only": b_only, "p_value": p}


def extract_score(val: Any) -> Optional[float]:
    if val is None:
        return None
    if isinstance(val, dict):
        s = val.get("score")
        return float(s) if s is not None else None
    return float(val)


def summarize_cells(cells: Sequence[Dict[str, Any]]) -> Dict[str, Any]:
    """Aggregate per-cell and pooled metrics across sweeps for each (strategy, agents)."""
    by_sa: Dict[Tuple[str, int], List[Dict[str, Any]]] = defaultdict(list)
    for c in cells:
        by_sa[(c["strategy"], c["agents"])].append(c)

    summary: Dict[str, Any] = {}
    for (strat, agents), group in sorted(by_sa.items()):
        all_turns = [t for c in group for t in c["turns"]]
        q_first = [t["question_to_first_ms"] / 1000.0 for t in all_turns]
        q_end = [t["question_to_end_ms"] / 1000.0 for t in all_turns]
        t1_first = [t["question_to_first_ms"] / 1000.0 for t in all_turns if t["turn"] == 1]
        t26_first = [t["question_to_first_ms"] / 1000.0 for t in all_turns if t["turn"] >= 2]

        sweep_p95_first = [
            percentile_nearest([t["question_to_first_ms"] / 1000.0 for t in c["turns"]], 95) for c in group
        ]
        sweep_p95_end = [
            percentile_nearest([t["question_to_end_ms"] / 1000.0 for t in c["turns"]], 95) for c in group
        ]
        tpms = [c["turns_per_min"] for c in group]

        scored_pairs = [(t, extract_score(t.get("score"))) for t in all_turns]
        scored = [(t, s) for t, s in scored_pairs if s is not None]
        by_cat: Dict[str, List[float]] = defaultdict(list)
        for t, s in scored:
            by_cat[str(t.get("category"))].append(s)

        tot_prompt = sum(t["prompt_tokens"] for t in all_turns)
        tot_cached = sum(t["cached_tokens"] for t in all_turns)
        tot_comp = [t["completion_tokens"] for t in all_turns]
        gpu_hits = sum((c.get("metrics_delta") or {}).get("vllm:prefix_cache_hits_total", 0.0) for c in group)
        gpu_queries = sum((c.get("metrics_delta") or {}).get("vllm:prefix_cache_queries_total", 0.0) for c in group)
        ext_hits = sum((c.get("metrics_delta") or {}).get("vllm:external_prefix_cache_hits_total", 0.0) for c in group)
        ext_queries = sum(
            (c.get("metrics_delta") or {}).get("vllm:external_prefix_cache_queries_total", 0.0) for c in group
        )

        summary[f"{strat}@n{agents}"] = {
            "strategy": strat,
            "agents": agents,
            "n_turns": len(all_turns),
            "turns_per_min_mean": sum(tpms) / len(tpms),
            "turns_per_min_by_sweep": tpms,
            "p95_first_sweep_avg_s": sum(sweep_p95_first) / len(sweep_p95_first),
            "p95_first_pooled_s": percentile_nearest(q_first, 95),
            "p95_first_ci95_s": bootstrap_ci(q_first, lambda xs: percentile_nearest(xs, 95)),
            "p95_end_sweep_avg_s": sum(sweep_p95_end) / len(sweep_p95_end),
            "p95_end_pooled_s": percentile_nearest(q_end, 95),
            "p95_turn1_pooled_s": percentile_nearest(t1_first, 95),
            "p95_turns2_6_pooled_s": percentile_nearest(t26_first, 95),
            "mean_prompt_tokens": (tot_prompt / len(all_turns)) if all_turns else 0.0,
            "cache_hit_pct": (100.0 * tot_cached / tot_prompt) if tot_prompt else 0.0,
            "gpu_prefix_hit_pct": (100.0 * gpu_hits / gpu_queries) if gpu_queries else 0.0,
            "ext_prefix_hit_pct": (100.0 * ext_hits / ext_queries) if ext_queries else 0.0,
            "ext_prefix_hits_total": ext_hits,
            "mean_completion_tokens": (sum(tot_comp) / len(tot_comp)) if tot_comp else 0.0,
            "fallback_rate": (
                sum(1 for t in all_turns if t.get("used_fallback")) / len(all_turns) if all_turns else 0.0
            ),
            "accuracy_all": (sum(s for _, s in scored) / len(scored)) if scored else None,
            "accuracy_by_cat": {k: sum(v) / len(v) for k, v in sorted(by_cat.items())},
            "errors": sum(1 for t in all_turns if t.get("error")),
        }
    return summary
