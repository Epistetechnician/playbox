"""One shared task, every channel metered in GPU-seconds.

The task ("project briefing"): a receiver agent serves sessions. Each session is about one project out of a shared corpus of project records
(32 facts per project, the kv_hybrid corpus) and has one TOPIC turn ("which field is Project P about?", answered from a short overview
chunk that says what the project is about) and several FACT turns ("what is the badge code for Project P?"). Every arm gets the same
sessions, the same receiver model and the same scoring; what differs is only how the sender's knowledge reaches the receiver:

    none         nothing (the floor: what the receiver's own prior gets)
    text_word    the topic as one text word in the context ("Topic: aviation.")          - carries the topic only
    text_rag     BM25 top-5 chunks for the turn's own question, fresh prefill every turn - carries topic and facts, if retrieval finds them
    hybrid       the project's working set (BM25 top-10 for "Project P") prefilled ONCE per session into a KV cache and reused every turn
    latent_q     the sender's thought mapped into the receiver and injected at its layer; the receiver answers the question prompt
    latent_free  the same vector, receiver continues an open prompt ("Lately I keep thinking about")   - carries the topic only
    swarm        topic turn by latent_free, fact turns by hybrid: the routed combination
    text_swarm   the same routing with the topic as a text word instead of a vector: what the latent route has to beat

Meter: every region that touches the model is timed with a synchronised clock (batch size 1, one request at a time, so GPU-seconds equal the
wall time of the region; a batched server would be cheaper per request for every arm). Phases: `think` (sender forward passes for the vector),
`cache_build` (the one prefill of a session's working set), `gen` (a receiver generate call: prefill of what is not cached plus decode).
The mapper fit (`fit_s`) is amortised over `amortise_sessions` sessions and added to the latent arms; text arms need no fit.

What a channel cannot carry scores zero on that turn type and the table says so: a steering vector carries a concept, not a badge code.
Scoring: topic turn = the generated text names the project's field (lexicon decoder, as everywhere in telepathy); fact turn = the exact
value appears in the generated text."""
from __future__ import annotations

import math
import random
import time
from collections import defaultdict
from contextlib import contextmanager

from .kv_hybrid import ATTRS, BM25, PREAMBLE, gen_corpus

ARMS = ("none", "text_word", "text_rag", "hybrid", "latent_q", "latent_free", "swarm", "text_swarm")
OVERVIEW_SENTENCES = 4
FACT_TURNS = 7
TOPIC_Q = "Which field is Project {p} about? Answer with one word."


def wilson(k: int, n: int, z: float = 1.96) -> tuple[float, float]:
    if n == 0:
        return 0.0, 0.0
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return max(0.0, c - h), min(1.0, c + h)


class Meter:
    """Synchronised wall-clock per (arm, phase). `sync` is called before and after each region (torch.cuda.synchronize on a GPU)."""

    def __init__(self, sync=lambda: None):
        self.sync, self.t, self.calls = sync, defaultdict(float), defaultdict(int)

    @contextmanager
    def track(self, arm: str, phase: str):
        self.sync()
        t0 = time.perf_counter()
        try:
            yield
        finally:
            self.sync()
            self.t[(arm, phase)] += time.perf_counter() - t0
            self.calls[(arm, phase)] += 1

    def arm_seconds(self, arm: str) -> dict:
        return {ph: round(s, 4) for (a, ph), s in self.t.items() if a == arm}


def build_world(n_sessions: int, count, concepts: dict, seed: int = 0, fact_turns: int = FACT_TURNS):
    """Corpus (n_sessions projects, one overview chunk each) and one session per project. `concepts` = {name: {"sentences": [...]}}.
    Returns (chunks, sessions); session = {"project", "concept", "turns": [{"kind": "topic"|"fact", "q", "answer"|"concept"}]}."""
    names = list(concepts)
    chunks, facts = gen_corpus(10 ** 9, count, seed=seed)  # every project the name list allows
    projects = []
    for c in chunks:
        if c["project"] not in projects:
            projects.append(c["project"])
    projects = projects[:n_sessions]
    assert len(projects) == n_sessions, f"the corpus has {len(projects)} projects, asked for {n_sessions}"
    keep = set(projects)
    chunks = [c for c in chunks if c["project"] in keep]
    rng = random.Random(seed + 1)
    sessions = []
    for i, p in enumerate(projects):
        concept = names[i % len(names)]
        text = f"[Document ov{i:03d}] Project {p}, overview.\n" + " ".join(concepts[concept]["sentences"][:OVERVIEW_SENTENCES])
        chunks.append({"id": len(chunks), "project": p, "text": text})
        attrs = rng.sample(ATTRS, fact_turns)
        turns = [{"kind": "topic", "q": TOPIC_Q.format(p=p), "concept": concept}]
        turns += [{"kind": "fact", "q": f"What is the {a} for Project {p}?", "answer": facts[(p, a)]} for a in attrs]
        sessions.append({"project": p, "concept": concept, "turns": turns})
    return chunks, sessions


def docs(chunks, idx) -> str:
    return "\n\n".join(chunks[i]["text"] for i in idx)


def prefix_of(chunks, idx) -> str:
    return PREAMBLE + "\n\nDocuments:\n\n" + docs(chunks, idx)


def suffix_of(q: str) -> str:
    return f"\n\nQuestion: {q}\nAnswer:"


def first_line(text: str) -> str:
    return text.strip().split("\n")[0]


def score(turn: dict, text: str, lexicons: dict, lexicon_id) -> bool:
    if turn["kind"] == "fact":
        return turn["answer"] in text
    pred, _ = lexicon_id(text, lexicons)
    return pred == turn["concept"]


def run_arm(arm: str, model_io, chunks, bm, sessions, concepts, meter: Meter, latent=None, rag_k: int = 5, hybrid_k: int = 10,
            gen_tokens: int = 16, lexicon_id=None) -> dict:
    """model_io: object with .ids(text, special=True)->tensor, .generate(ids, n, cache=None, vec=None, prefix_len=0)->text, .prefill(ids)->cache,
    .crop(cache, n). latent: object with .vector(concept)->tensor (the mapped, scaled direction) and .free_prompts. Returns raw per-turn results."""
    lex = {c: set(v["lexicon"]) for c, v in concepts.items()}
    rows = []
    for si, s in enumerate(sessions):
        cache = pref_ids = vec = None
        need_hybrid = arm in ("hybrid", "swarm", "text_swarm")
        if need_hybrid:
            idx = sorted(bm.top(f"Project {s['project']}", hybrid_k))
            pref_ids = model_io.ids(prefix_of(chunks, idx))
            with meter.track(arm, "cache_build"):
                cache = model_io.prefill(pref_ids)
        if arm in ("latent_q", "latent_free", "swarm"):
            with meter.track(arm, "think"):
                vec = latent.vector(s["concept"])
        for ti, t in enumerate(s["turns"]):
            q = t["q"]
            use_vec = None
            cache_t, pre = None, None
            if arm == "none":
                text = f"Question: {q}\nAnswer:"
            elif arm == "text_word" or (arm == "text_swarm" and t["kind"] == "topic"):
                note = f"Topic: {s['concept']}.\n" if t["kind"] == "topic" else ""
                text = note + f"Question: {q}\nAnswer:"
            elif arm == "text_rag":
                text = prefix_of(chunks, sorted(bm.top(q, rag_k))) + suffix_of(q)
            elif arm == "hybrid" or (arm in ("swarm", "text_swarm") and t["kind"] == "fact"):
                cache_t, pre = cache, pref_ids
                text = suffix_of(q)
            elif arm == "latent_q":
                text, use_vec = f"Question: {q}\nAnswer:", (vec if t["kind"] == "topic" else None)
            elif arm in ("latent_free", "swarm"):  # swarm topic turn
                text = latent.free_prompts[si % len(latent.free_prompts)] if t["kind"] == "topic" else f"Question: {q}\nAnswer:"
                use_vec = vec if t["kind"] == "topic" else None
            else:
                raise ValueError(arm)
            ids = model_io.ids(text, special=pre is None)
            n_ctx = int(ids.shape[-1]) + (int(pre.shape[-1]) if pre is not None else 0)
            with meter.track(arm, "gen"):
                out = model_io.generate(ids, gen_tokens, cache=cache_t, vec=use_vec, prefix_ids=pre)
            out = first_line(out) if t["kind"] == "fact" else out
            rows.append({"arm": arm, "session": si, "turn": ti, "kind": t["kind"], "ok": bool(score(t, out, lex, lexicon_id)), "ctx_tokens": n_ctx,
                         "out": out[:80]})
    return {"rows": rows}


def summarize(res: dict, meter: Meter, n_sessions: int, rate_usd_h: float, fit_s: float = 0.0, amortise_sessions: int = 1000,
              arms=ARMS) -> dict:
    out = {}
    for arm in arms:
        if arm not in res:
            continue
        rows = res[arm]["rows"]
        sec = meter.arm_seconds(arm)
        direct = sum(sec.values())
        fit = fit_s / amortise_sessions * n_sessions if arm in ("latent_q", "latent_free", "swarm") else 0.0
        total_s = direct + fit
        row = {"gpu_s_phases": sec, "fit_amortised_s": round(fit, 4), "gpu_s_total": round(total_s, 4),
               "gpu_s_per_session": round(total_s / n_sessions, 4), "usd_per_1000_sessions": round(1000 * total_s / n_sessions * rate_usd_h / 3600, 4),
               "ctx_tokens_per_turn": round(sum(r["ctx_tokens"] for r in rows) / len(rows), 1)}
        ok_all = 0
        for kind in ("topic", "fact"):
            rs = [r for r in rows if r["kind"] == kind]
            k = sum(r["ok"] for r in rs)
            lo, hi = wilson(k, len(rs))
            row[kind] = {"n": len(rs), "correct": k, "acc": round(k / len(rs), 4), "ci95": [round(lo, 3), round(hi, 3)]}
            ok_all += k
        row["correct_total"] = ok_all
        usd = total_s * rate_usd_h / 3600
        row["correct_per_usd"] = round(ok_all / usd, 1) if usd > 0 else None
        out[arm] = row
    return out
