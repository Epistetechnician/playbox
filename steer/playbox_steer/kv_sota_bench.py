"""SOTA benchmark adapters (LoCoMo + FinanceBench) for routed KV vs RAG vs shared KV.

Supports:
- LoCoMo (snap-research/locomo): 10 long multi-session conversations (~19K tokens each),
  evaluating S-RAG, S-CONV-RAG (with live LLM query rewriter), and S-ROUTED (per-conversation KV).
- FinanceBench (PatronusAI/financebench): 150 enterprise 10-K/10-Q questions across 32 companies
  (~3.5K tokens/company, ~111K shared corpus), evaluating S-CTX (shared ~111K KV), S-RAG, and S-ROUTED.
"""
from __future__ import annotations

import json
import math
import re
import string
import time
import uuid
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Tuple

from playbox_steer.kv_routed import (
    EntityRouter,
    mcnemar_exact,
    percentile_nearest as percentile,
    scrape_vllm_metrics as scrape_kv_metrics,
    stream_chat_completion,
)


def _norm_text(s: str) -> str:
    s = str(s).lower()
    s = re.sub(r"\b(a|an|the)\b", " ", s)
    s = "".join(ch if ch not in string.punctuation else " " for ch in s)
    return " ".join(s.split())


def token_f1(pred: str, gold: str) -> float:
    p_toks = _norm_text(pred).split()
    g_toks = _norm_text(gold).split()
    if not p_toks or not g_toks:
        return 1.0 if p_toks == g_toks else 0.0
    common = Counter(p_toks) & Counter(g_toks)
    num_same = sum(common.values())
    if num_same == 0:
        return 0.0
    prec = num_same / len(p_toks)
    rec = num_same / len(g_toks)
    return 2.0 * prec * rec / (prec + rec)


def _extract_answer_span(text: str) -> str:
    m = re.search(r"(?:^|\n)\s*Answer\s*:\s*(.+)", text, re.IGNORECASE)
    if m:
        return m.group(1).strip()
    lines = [ln.strip() for ln in text.strip().splitlines() if ln.strip()]
    return lines[-1] if lines else text.strip()


def score_locomo_turn(pred: str, gold: Any) -> Tuple[float, int]:
    """Return (token_f1, binary_hit) matching standard LoCoMo F1 + relaxed exact/substring hit."""
    ans = _extract_answer_span(pred)
    g_str = str(gold).strip()
    f1 = max(token_f1(ans, g_str), token_f1(pred, g_str))
    na, ng = _norm_text(ans), _norm_text(g_str)
    hit = 1 if (ng and (ng in na or ng in _norm_text(pred) or f1 >= 0.5)) else 0
    return f1, hit


_NUM_RE = re.compile(r"-?\$?\d[\d,]*(?:\.\d+)?%?")


def _parse_numbers(text: str) -> List[float]:
    nums: List[float] = []
    for tok in _NUM_RE.findall(text):
        cleaned = tok.replace("$", "").replace(",", "").replace("%", "")
        try:
            nums.append(float(cleaned))
        except ValueError:
            continue
    return nums


def score_financebench_turn(pred: str, gold: str) -> Tuple[float, int]:
    """Score FinanceBench using numeric tolerance (+-2%) for quantitative answers and F1/verdict for qualitative."""
    ans = _extract_answer_span(pred)
    g_str = str(gold).strip()
    f1 = max(token_f1(ans, g_str), token_f1(pred, g_str))
    g_nums = _parse_numbers(g_str)
    if g_nums:
        p_nums = _parse_numbers(ans) + _parse_numbers(pred)
        primary = g_nums[0]
        for pn in p_nums:
            if primary == 0:
                if abs(pn) <= 1e-3:
                    return max(f1, 1.0), 1
            else:
                if abs(pn - primary) / abs(primary) <= 0.025:
                    return max(f1, 1.0), 1
                # Also handle billion vs million scaling (e.g. 1.58B vs 1577M)
                for scale in (1e3, 1e-3, 100.0, 0.01):
                    if abs(pn * scale - primary) / abs(primary) <= 0.025:
                        return max(f1, 1.0), 1
        return f1, 0
    na, ng = _norm_text(ans), _norm_text(g_str)
    # Yes/No verdict check
    g_first = ng.split()[0] if ng else ""
    if g_first in ("yes", "no"):
        p_first = na.split()[0] if na else ""
        if p_first == g_first:
            return max(f1, 1.0), 1
        return f1, 0
    hit = 1 if (f1 >= 0.35 or (ng and ng in na)) else 0
    return f1, hit


class BM25ChunkRetriever:
    """Self-contained pure-Python Okapi BM25 retriever (zero external dependencies)."""

    def __init__(self, chunks: List[Tuple[str, str]], top_k: int = 5, k1: float = 1.5, b: float = 0.75) -> None:
        self.chunks = chunks
        self.top_k = top_k
        self.k1 = k1
        self.b = b
        self._doc_tfs: List[Counter[str]] = []
        self._doc_lens: List[int] = []
        df: Counter[str] = Counter()
        for _, txt in chunks:
            toks = re.findall(r"[a-z0-9]+", txt.lower())
            tf = Counter(toks)
            self._doc_tfs.append(tf)
            self._doc_lens.append(len(toks))
            for t in tf:
                df[t] += 1
        n_docs = max(len(chunks), 1)
        self._avgdl = sum(self._doc_lens) / n_docs if self._doc_lens else 1.0
        self._idf = {t: math.log((n_docs - freq + 0.5) / (freq + 0.5) + 1.0) for t, freq in df.items()}

    def __call__(self, query: str) -> Tuple[List[str], Dict[str, float]]:
        t0 = time.perf_counter()
        q_toks = re.findall(r"[a-z0-9]+", query.lower())
        scores: List[Tuple[float, int]] = []
        for idx, tf in enumerate(self._doc_tfs):
            dl = self._doc_lens[idx]
            denom_norm = self.k1 * (1.0 - self.b + self.b * (dl / max(self._avgdl, 1e-6)))
            s = 0.0
            for qt in q_toks:
                f = tf.get(qt, 0)
                if f:
                    s += self._idf.get(qt, 0.0) * (f * (self.k1 + 1.0)) / (f + denom_norm)
            scores.append((s, idx))
        scores.sort(key=lambda kv: kv[0], reverse=True)
        docs = [self.chunks[idx][1] for _, idx in scores[: self.top_k]]
        elapsed_ms = (time.perf_counter() - t0) * 1000.0
        return docs, {"retrieve_ms": elapsed_ms}


class DenseRerankChunkRetriever:
    """Two-stage dense bi-encoder + cross-encoder reranker retriever.

    Uses ``BAAI/bge-small-en-v1.5`` + ``BAAI/bge-reranker-v2-m3`` by default via
    ``sentence-transformers`` (runs on CPU/CUDA cleanly alongside vLLM), or accepts
    custom ``embed_fn`` / ``rerank_fn`` callables for fast offline testing.
    """

    def __init__(
        self,
        chunks: List[Tuple[str, str]],
        top_k: int = 5,
        cand_k: int = 20,
        embed_model: str = "BAAI/bge-small-en-v1.5",
        rerank_model: str = "BAAI/bge-reranker-v2-m3",
        device: str = "cpu",
        hybrid_bm25: bool = True,
        embed_fn: Optional[Callable[[List[str]], List[List[float]]]] = None,
        rerank_fn: Optional[Callable[[List[Tuple[str, str]]], List[float]]] = None,
    ) -> None:
        import threading

        self.chunks = chunks
        self.top_k = top_k
        self.cand_k = max(cand_k, top_k)
        self.hybrid_bm25 = hybrid_bm25
        self._lock = threading.Lock()
        self._texts = [txt for _, txt in chunks]
        self._bm25 = BM25ChunkRetriever(chunks, top_k=self.cand_k) if hybrid_bm25 and chunks else None

        if embed_fn is not None:
            self._embed_fn = embed_fn
        else:
            import torch
            from sentence_transformers import SentenceTransformer

            mk = {"dtype": torch.float16} if device.startswith("cuda") else {}
            enc = SentenceTransformer(embed_model, device=device, model_kwargs=mk)
            self._embed_fn = lambda xs: enc.encode(xs, normalize_embeddings=True, batch_size=64, show_progress_bar=False).tolist()

        if rerank_fn is not None:
            self._rerank_fn = rerank_fn
        else:
            import torch
            from sentence_transformers import CrossEncoder

            mk = {"dtype": torch.float16} if device.startswith("cuda") else {}
            rr = CrossEncoder(rerank_model, device=device, model_kwargs=mk)
            self._rerank_fn = lambda pairs: [float(x) for x in rr.predict(pairs, batch_size=32, show_progress_bar=False)]

        raw_vecs = self._embed_fn(self._texts) if self._texts else []
        self._doc_vecs = [self._l2_norm(v) for v in raw_vecs]

    @staticmethod
    def _l2_norm(vec: List[float]) -> List[float]:
        norm = math.sqrt(sum(x * x for x in vec)) or 1.0
        return [x / norm for x in vec]

    def __call__(self, query: str) -> Tuple[List[str], Dict[str, float]]:
        t0 = time.perf_counter()
        if not self._texts:
            return [], {"retrieve_ms": 0.0, "embed_ms": 0.0, "rerank_ms": 0.0}
        with self._lock:
            q_vec = self._l2_norm(self._embed_fn([query])[0])
            t_emb = time.perf_counter()
            sims = [
                (sum(a * b for a, b in zip(q_vec, dv)), idx)
                for idx, dv in enumerate(self._doc_vecs)
            ]
            sims.sort(key=lambda kv: kv[0], reverse=True)
            if self._bm25 is not None:
                # Reciprocal Rank Fusion (BM25 + Dense) for candidate pool before cross-encoder rerank
                q_toks = re.findall(r"[a-z0-9]+", query.lower())
                bm_scores: List[Tuple[float, int]] = []
                for idx, tf in enumerate(self._bm25._doc_tfs):
                    dl = self._bm25._doc_lens[idx]
                    denom_norm = self._bm25.k1 * (1.0 - self._bm25.b + self._bm25.b * (dl / max(self._bm25._avgdl, 1e-6)))
                    s = sum(
                        self._bm25._idf.get(qt, 0.0) * (tf[qt] * (self._bm25.k1 + 1.0)) / (tf[qt] + denom_norm)
                        for qt in q_toks
                        if qt in tf
                    )
                    bm_scores.append((s, idx))
                bm_scores.sort(key=lambda kv: kv[0], reverse=True)
                rrf: Dict[int, float] = {}
                for rank, (_, idx) in enumerate(sims[: self.cand_k * 2]):
                    rrf[idx] = rrf.get(idx, 0.0) + 1.0 / (60.0 + rank + 1.0)
                for rank, (_, idx) in enumerate(bm_scores[: self.cand_k * 2]):
                    rrf[idx] = rrf.get(idx, 0.0) + 1.0 / (60.0 + rank + 1.0)
                cand_indices = [idx for idx, _ in sorted(rrf.items(), key=lambda kv: kv[1], reverse=True)[: self.cand_k]]
            else:
                cand_indices = [idx for _, idx in sims[: self.cand_k]]
            pairs = [(query, self._texts[idx][:2000]) for idx in cand_indices]
            rr_scores = self._rerank_fn(pairs)
            t_rr = time.perf_counter()
        ranked = sorted(zip(rr_scores, cand_indices), key=lambda kv: kv[0], reverse=True)
        docs = [self._texts[idx] for _, idx in ranked[: self.top_k]]
        return docs, {
            "retrieve_ms": (t_rr - t0) * 1000.0,
            "embed_ms": (t_emb - t0) * 1000.0,
            "rerank_ms": (t_rr - t_emb) * 1000.0,
        }


@dataclass
class SotaSession:
    session_id: str
    entity_id: str
    entity_name: str
    turns: List[Dict[str, Any]]  # [{qid, category, question, answer}]


@dataclass
class SotaDataset:
    name: str
    entities: Dict[str, str]  # entity_id -> entity_name
    aliases: Dict[str, List[str]]  # entity_id -> extra search aliases
    dossiers: Dict[str, str]  # entity_id -> full working-set dossier text
    chunks: List[Tuple[str, str]]  # [(chunk_id, chunk_text)] for RAG
    sessions: List[SotaSession]
    shared_corpus: str  # concatenated corpus for S-CTX (if applicable)


def load_locomo(path: Path, max_per_conv: int = 30, turns_per_session: int = 6) -> SotaDataset:
    """Load LoCoMo (Cats 1-4) into per-conversation working sets and multi-turn sessions."""
    raw = json.loads(path.read_text(encoding="utf-8"))
    entities: Dict[str, str] = {}
    aliases: Dict[str, List[str]] = {}
    dossiers: Dict[str, str] = {}
    chunks: List[Tuple[str, str]] = []
    sessions: List[SotaSession] = []

    for idx, item in enumerate(raw):
        eid = f"conv-{idx:02d}"
        cv = item["conversation"]
        sp_a = str(cv.get("speaker_a", f"SpeakerA{idx}")).strip()
        sp_b = str(cv.get("speaker_b", f"SpeakerB{idx}")).strip()
        ename = f"{sp_a} & {sp_b}"
        entities[eid] = ename
        aliases[eid] = [sp_a, sp_b]

        skeys = sorted(
            [k for k in cv if k.startswith("session_") and not k.endswith("_date_time")],
            key=lambda s: int(s.split("_")[1]),
        )
        sess_blocks: List[str] = []
        for sk in skeys:
            dt = cv.get(f"{sk}_date_time", "")
            header = f"=== Conversation {ename} | {sk} ({dt}) ==="
            lines = [header]
            for turn in cv[sk]:
                sp = turn.get("speaker", "")
                txt = turn.get("text", "")
                lines.append(f"{sp}: {txt}")
            block = "\n".join(lines)
            sess_blocks.append(block)
            chunks.append((f"{eid}:{sk}", block))
        dossiers[eid] = "\n\n".join(sess_blocks)

        # Balanced sample across Categories 1-4 (exclude Cat 5 adversarial which lacks ground-truth answer)
        by_cat: Dict[int, List[Dict[str, Any]]] = {1: [], 2: [], 3: [], 4: []}
        for q_idx, qa in enumerate(item["qa"]):
            cat = int(qa.get("category", 0))
            if cat in by_cat and "answer" in qa:
                by_cat[cat].append(
                    {
                        "qid": f"{eid}-q{q_idx:03d}",
                        "category": f"Cat{cat}",
                        "question": str(qa["question"]),
                        "answer": str(qa["answer"]),
                    }
                )
        picked: List[Dict[str, Any]] = []
        # Round-robin across categories for balanced representation
        ptrs = {c: 0 for c in (1, 2, 3, 4)}
        while len(picked) < max_per_conv:
            progressed = False
            for c in (1, 2, 3, 4):
                if ptrs[c] < len(by_cat[c]) and len(picked) < max_per_conv:
                    picked.append(by_cat[c][ptrs[c]])
                    ptrs[c] += 1
                    progressed = True
            if not progressed:
                break

        for s_off in range(0, len(picked), turns_per_session):
            chunk_q = picked[s_off : s_off + turns_per_session]
            if chunk_q:
                sessions.append(
                    SotaSession(
                        session_id=f"{eid}-s{s_off // turns_per_session}",
                        entity_id=eid,
                        entity_name=ename,
                        turns=chunk_q,
                    )
                )

    shared = "\n\n".join(dossiers[k] for k in sorted(dossiers))
    return SotaDataset(
        name="locomo",
        entities=entities,
        aliases=aliases,
        dossiers=dossiers,
        chunks=chunks,
        sessions=sessions,
        shared_corpus=shared,
    )


def load_financebench(path: Path) -> SotaDataset:
    """Load FinanceBench (150 questions across 32 companies) into per-company filing dossiers."""
    rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    by_co: Dict[str, List[Dict[str, Any]]] = {}
    for r in rows:
        by_co.setdefault(str(r["company"]).strip(), []).append(r)

    entities: Dict[str, str] = {}
    aliases: Dict[str, List[str]] = {}
    dossiers: Dict[str, str] = {}
    chunks: List[Tuple[str, str]] = []
    sessions: List[SotaSession] = []

    for idx, co in enumerate(sorted(by_co)):
        eid = f"co-{idx:02d}"
        entities[eid] = co
        aliases[eid] = [co]
        seen_pages: Dict[Tuple[str, Any], str] = {}
        turns: List[Dict[str, Any]] = []
        for r in by_co[co]:
            for ev in r.get("evidence", []):
                doc_name = str(ev.get("doc_name") or r.get("doc_name") or "")
                pg = ev.get("evidence_page_num", -1)
                txt = str(ev.get("evidence_text_full_page") or ev.get("evidence_text") or "").strip()
                if txt and (doc_name, pg) not in seen_pages:
                    seen_pages[(doc_name, pg)] = txt
            turns.append(
                {
                    "qid": str(r["financebench_id"]),
                    "category": str(r.get("question_type", "unknown")),
                    "question": str(r["question"]),
                    "answer": str(r["answer"]),
                }
            )
        page_blocks: List[str] = []
        for (doc_name, pg), txt in sorted(seen_pages.items(), key=lambda kv: (kv[0][0], str(kv[0][1]))):
            blk = f"=== Company: {co} | Filing: {doc_name} | Page {pg} ===\n{txt}"
            page_blocks.append(blk)
            chunks.append((f"{eid}:{doc_name}:p{pg}", blk))
        dossiers[eid] = "\n\n".join(page_blocks)
        sessions.append(SotaSession(session_id=f"{eid}-s0", entity_id=eid, entity_name=co, turns=turns))

    shared = "\n\n".join(dossiers[k] for k in sorted(dossiers))
    return SotaDataset(
        name="financebench",
        entities=entities,
        aliases=aliases,
        dossiers=dossiers,
        chunks=chunks,
        sessions=sessions,
        shared_corpus=shared,
    )


class SotaEntityRouter:
    """Entity router supporting speaker-pair aliases (LoCoMo) and company names (FinanceBench)."""

    def __init__(self, entities: Dict[str, str], aliases: Dict[str, List[str]]) -> None:
        self.entities = entities
        alias_counts: Counter[str] = Counter()
        for eid, al_list in aliases.items():
            for al in al_list:
                norm = al.strip().lower()
                if norm:
                    alias_counts[norm] += 1
        self._unique_aliases: List[Tuple[str, str]] = []
        for eid, al_list in aliases.items():
            for al in al_list:
                norm = al.strip().lower()
                if norm and alias_counts[norm] == 1:
                    self._unique_aliases.append((norm, eid))
        self._unique_aliases.sort(key=lambda kv: len(kv[0]), reverse=True)

    def resolve(self, question: str, active_entity: Optional[str] = None) -> Tuple[Optional[str], str]:
        q_low = question.lower()
        hits: List[str] = []
        for al, eid in self._unique_aliases:
            if re.search(rf"\b{re.escape(al)}\b", q_low):
                if eid not in hits:
                    hits.append(eid)
        if len(hits) == 1:
            return hits[0], "entity_hit"
        if len(hits) > 1:
            return None, "rag_fallback"
        if active_entity:
            return active_entity, "session_carry"
        return None, "rag_fallback"


def rewrite_query_with_llm(
    base_url: str,
    model: str,
    question: str,
    history: List[Tuple[str, str]],
    timeout_s: float = 60.0,
) -> Tuple[str, float]:
    """Run a lightweight LLM query-rewriter turn for S-CONV-RAG (timed end-to-end)."""
    if not history:
        return question, 0.0
    hist_txt = "\n".join(f"Q: {q}\nA: {a[:120]}" for q, a in history[-2:])
    msgs = [
        {
            "role": "system",
            "content": "Rewrite the follow-up question into a standalone search query resolving pronouns and speaker/company names. Output ONLY the rewritten query.",
        },
        {"role": "user", "content": f"Prior turns:\n{hist_txt}\n\nFollow-up question: {question}\nStandalone query:"},
    ]
    t0 = time.perf_counter()
    res = stream_chat_completion(base_url, model, msgs, max_tokens=48, timeout=timeout_s)
    elapsed_ms = (time.perf_counter() - t0) * 1000.0
    rewritten = res["output"].strip() or question
    return rewritten, elapsed_ms


def run_sota_cell(
    dataset: SotaDataset,
    strategy: str,
    concurrency: int,
    block: int,
    base_url: str,
    model: str,
    retriever: BM25ChunkRetriever,
    max_tokens: int = 128,
    timeout_s: float = 180.0,
) -> Dict[str, Any]:
    """Run one cache-isolated SOTA benchmark cell (S-RAG, S-CONV-RAG, S-CTX, or S-ROUTED)."""
    assert strategy in ("S-RAG", "S-CONV-RAG", "S-CTX", "S-ROUTED"), f"Unknown strategy: {strategy}"
    salt = f"[cell-salt:{uuid.uuid4().hex}]"
    router = SotaEntityRouter(dataset.entities, dataset.aliases)
    score_fn: Callable[[str, Any], Tuple[float, int]] = (
        score_locomo_turn if dataset.name == "locomo" else score_financebench_turn
    )
    sys_instr = (
        "Answer the question concisely and accurately using ONLY the provided context. "
        "Include the exact date, number, entity, or short phrase in a final line formatted as `Answer: <answer>`."
    )

    kv_before = scrape_kv_metrics(base_url)

    def _run_session(sess: SotaSession) -> List[Dict[str, Any]]:
        out_turns: List[Dict[str, Any]] = []
        active_entity: Optional[str] = None
        history: List[Tuple[str, str]] = []
        for t_idx, q in enumerate(sess.turns, start=1):
            question = q["question"]
            t_arrival = time.perf_counter()
            route_mode = "none"
            retrieve_ms = 0.0
            rewrite_ms = 0.0
            fallback = False

            if strategy == "S-CTX":
                msgs = [
                    {"role": "system", "content": f"{salt}\n{sys_instr}\n\n{dataset.shared_corpus}"},
                    {"role": "user", "content": question},
                ]
            elif strategy in ("S-RAG", "S-CONV-RAG"):
                search_q = question
                if strategy == "S-CONV-RAG" and history:
                    search_q, rewrite_ms = rewrite_query_with_llm(base_url, model, question, history, timeout_s=timeout_s)
                docs, meta = retriever(search_q)
                retrieve_ms = float(meta.get("retrieve_ms", 0.0))
                ctx = "\n\n---\n\n".join(docs)
                msgs = [
                    {"role": "system", "content": f"{salt}\n{sys_instr}"},
                    {"role": "user", "content": f"Retrieved context:\n{ctx}\n\nQuestion: {question}"},
                ]
            else:
                # S-ROUTED
                eid, route_mode = router.resolve(question, active_entity=active_entity)
                if eid is not None:
                    active_entity = eid
                    msgs = [
                        {"role": "system", "content": f"{salt}\n{sys_instr}\n\n{dataset.dossiers[eid]}"},
                        {"role": "user", "content": question},
                    ]
                else:
                    fallback = True
                    docs, meta = retriever(question)
                    retrieve_ms = float(meta.get("retrieve_ms", 0.0))
                    ctx = "\n\n---\n\n".join(docs)
                    msgs = [
                        {"role": "system", "content": f"{salt}\n{sys_instr}"},
                        {"role": "user", "content": f"Retrieved context:\n{ctx}\n\nQuestion: {question}"},
                    ]

            t_pre_ms = (time.perf_counter() - t_arrival) * 1000.0
            res = stream_chat_completion(base_url, model, msgs, max_tokens=max_tokens, timeout=timeout_s)
            q_to_first_ms = t_pre_ms + res["ttft_ms"]
            q_to_end_ms = t_pre_ms + res["total_ms"]
            f1, hit = score_fn(res["output"], q["answer"])
            history.append((question, _extract_answer_span(res["output"])))

            out_turns.append(
                {
                    "session_id": sess.session_id,
                    "turn_idx": t_idx,
                    "qid": q["qid"],
                    "category": q["category"],
                    "gold_entity": sess.entity_id,
                    "routed_entity": active_entity if strategy == "S-ROUTED" else None,
                    "route_mode": route_mode,
                    "fallback": fallback,
                    "question_to_first_ms": q_to_first_ms,
                    "question_to_end_ms": q_to_end_ms,
                    "retrieve_ms": retrieve_ms,
                    "rewrite_ms": rewrite_ms,
                    "prompt_tokens": res["prompt_tokens"],
                    "completion_tokens": res["completion_tokens"],
                    "cached_tokens": res["cached_tokens"],
                    "finish_reason": res["finish_reason"],
                    "error": res["error"],
                    "f1": f1,
                    "hit": hit,
                    "gold": q["answer"],
                    "text": res["output"],
                }
            )
        return out_turns

    t_wall0 = time.perf_counter()
    records: List[Dict[str, Any]] = []
    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        futs = [pool.submit(_run_session, s) for s in dataset.sessions]
        for fut in as_completed(futs):
            records.extend(fut.result())
    wall_s = time.perf_counter() - t_wall0
    kv_after = scrape_kv_metrics(base_url)

    n = len(records)
    tot_prompt = sum(r["prompt_tokens"] for r in records)
    tot_cached = sum(r["cached_tokens"] for r in records)
    q_first = [r["question_to_first_ms"] / 1000.0 for r in records]
    q_end = [r["question_to_end_ms"] / 1000.0 for r in records]
    t2_first = [r["question_to_first_ms"] / 1000.0 for r in records if r["turn_idx"] >= 2]
    f1_mean = sum(r["f1"] for r in records) / max(n, 1)
    acc = sum(r["hit"] for r in records) / max(n, 1)
    by_cat: Dict[str, Dict[str, float]] = {}
    for cat in sorted({r["category"] for r in records}):
        cr = [r for r in records if r["category"] == cat]
        by_cat[cat] = {
            "n": len(cr),
            "f1": sum(r["f1"] for r in cr) / len(cr),
            "acc": sum(r["hit"] for r in cr) / len(cr),
        }

    def _delta(k: str) -> float:
        a, b = kv_after.get(k, 0.0), kv_before.get(k, 0.0)
        return float(a) - float(b) if isinstance(a, (int, float)) and isinstance(b, (int, float)) else 0.0

    gpu_q = _delta("vllm:prefix_cache_queries_total")
    gpu_h = _delta("vllm:prefix_cache_hits_total")
    ext_q = _delta("vllm:external_prefix_cache_queries_total")
    ext_h = _delta("vllm:external_prefix_cache_hits_total")

    summary = {
        "dataset": dataset.name,
        "strategy": strategy,
        "concurrency": concurrency,
        "block": block,
        "n_turns": n,
        "errors": sum(1 for r in records if r["error"]),
        "wall_s": wall_s,
        "turns_per_min": (n / wall_s) * 60.0 if wall_s > 0 else 0.0,
        "p95_q_first_s": percentile(q_first, 95),
        "p95_q_end_s": percentile(q_end, 95),
        "p95_t2_first_s": percentile(t2_first, 95) if t2_first else 0.0,
        "mean_prompt_tokens": tot_prompt / max(n, 1),
        "mean_completion_tokens": sum(r["completion_tokens"] for r in records) / max(n, 1),
        "cache_hit_pct": (100.0 * tot_cached / tot_prompt) if tot_prompt > 0 else 0.0,
        "gpu_prefix_hit_pct": (100.0 * gpu_h / gpu_q) if gpu_q > 0 else 0.0,
        "ext_prefix_hit_pct": (100.0 * ext_h / ext_q) if ext_q > 0 else 0.0,
        "ext_prefix_hits_total": ext_h,
        "fallback_rate": sum(1 for r in records if r["fallback"]) / max(n, 1),
        "mean_rewrite_ms": sum(r["rewrite_ms"] for r in records) / max(n, 1),
        "mean_retrieve_ms": sum(r["retrieve_ms"] for r in records) / max(n, 1),
        "f1": f1_mean,
        "accuracy": acc,
        "by_cat": by_cat,
    }
    return {"summary": summary, "turns": records}


def paired_mcnemar_sota(turns_a: List[Dict[str, Any]], turns_b: List[Dict[str, Any]]) -> Dict[str, Any]:
    by_a = {t["qid"]: float(t["hit"]) for t in turns_a}
    by_b = {t["qid"]: float(t["hit"]) for t in turns_b}
    return mcnemar_exact(by_a, by_b)
