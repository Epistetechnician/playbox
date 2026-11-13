"""Cached full context against RAG against retrieval-then-cache ("hybrid"), on one shared 128K-token corpus (stdlib only).

The synthetic corpus is 80 projects x 8 chunks. Every chunk states four facts about its project ("For Project Aldebaran-Nine,
the badge code is QX-4821-M.") among filler sentences; the same 32 attributes recur for every project, with different random
values, so a lookup needs the right project AND the right attribute and a near-miss project (Aldebaran-Seven) is a real distractor.
Ground truth is the value string.

Workload: S concurrent agent sessions, each about one project, asking T questions in turn (different attributes).

Arms (the prompt is always: shared preamble, then context, then the question):
    full          the whole corpus is the context (server run with prefix caching off, or on after one warm request)
    rag           per question: BM25 top-5 chunks of the whole corpus (the usual RAG; the context changes every turn)
    hybrid        per session: BM25 top-K chunks for the project name, in document order, retrieved ONCE and kept as a stable
                  prefix, so prefix caching reuses it on every later turn and across agents on the same project

Reported per arm: exact-substring accuracy, time to first token (turn 1 and later turns separately), end-to-end latency,
turns per minute at S concurrent agents, prompt tokens and how many were served from cache, and GPU seconds per correct answer."""
from __future__ import annotations

import json
import math
import random
import re
import threading
import time
import urllib.request
from collections import Counter
from concurrent.futures import ThreadPoolExecutor

STARS = ["Aldebaran", "Betelgeuse", "Canopus", "Deneb", "Electra", "Fomalhaut", "Gacrux", "Hadar", "Izar", "Jabbah",
         "Kochab", "Lesath", "Mirfak", "Nunki", "Okab", "Procyon"]
NUMS = ["One", "Three", "Five", "Seven", "Nine"]
ATTRS = ["badge code", "launch date", "budget code", "lead engineer", "primary vendor", "ticket number", "server rack",
         "cost centre", "safety officer", "test lab", "firmware build", "contract number", "site code", "radio channel",
         "review board", "insurance policy", "patent filing", "shipping lane", "warehouse bay", "audit reference",
         "grant number", "license key", "cable batch", "paint colour code", "tool serial", "vehicle plate", "gate pass",
         "sensor model", "backup tape", "escrow account", "customs form", "survey plot"]
FILLER = ["The weekly review covered scheduling, staffing and the usual list of small open items.",
          "Several teams reported that the {n} area was quiet and no incidents had been logged.",
          "A note from facilities said the {n} corridor would be repainted over the next few weeks.",
          "Minutes were circulated afterwards, though few people read past the first page.",
          "The {n} committee asked for a clearer summary of progress ahead of the next quarter.",
          "Travel plans for the {n} visit were postponed until the weather settled.",
          "An external reviewer praised the documentation and asked for more diagrams of the {n} layout.",
          "Procurement confirmed that the {n} order was still in the queue behind two others.",
          "No changes were made to the {n} policy during this period.",
          "Attendance was good, and the {n} discussion ran longer than planned."]
NOUNS = ["north", "harbour", "annex", "depot", "atrium", "workshop", "campus", "terminal", "gallery", "yard"]
WORDS = re.compile(r"[a-z0-9]+")


def _value(rng: random.Random) -> str:
    return f"{rng.choice('QXKZWRTP')}{rng.choice('QXKZWRTP')}-{rng.randint(1000, 9999)}-{rng.choice('ABCDEFGH')}"


def gen_corpus(target_tokens: int, count, seed: int = 0, per_chunk_facts: int = 4):
    """Returns (chunks, facts): chunks = [{"id", "project", "text"}], facts = {(project, attr): value}. `count(text)` is a token counter."""
    rng = random.Random(seed)
    names = [f"{s}-{n}" for s in STARS for n in NUMS]
    rng.shuffle(names)
    chunks, facts, total, i = [], {}, 0, 0
    per_project = len(ATTRS) // per_chunk_facts  # 8 chunks of 4 facts cover the 32 attributes
    pr = 0
    while total < target_tokens and pr < len(names):
        name = names[pr]
        order = list(range(len(ATTRS)))
        rng.shuffle(order)
        for k in range(per_project):
            lines = []
            for a in order[k * per_chunk_facts:(k + 1) * per_chunk_facts]:
                v = _value(rng)
                facts[(name, ATTRS[a])] = v
                lines.append(f"For Project {name}, the {ATTRS[a]} is {v}.")
            for _ in range(5):
                lines.insert(rng.randint(0, len(lines)), rng.choice(FILLER).format(n=rng.choice(NOUNS)))
            text = f"[Document {i:04d}] Project {name}, section {k + 1}.\n" + " ".join(lines)
            chunks.append({"id": i, "project": name, "text": text})
            total += count(text)
            i += 1
        pr += 1
    return chunks, facts


class BM25:
    def __init__(self, docs: list[str], k1: float = 1.5, b: float = 0.75):
        self.toks = [WORDS.findall(d.lower()) for d in docs]
        self.k1, self.b = k1, b
        self.avg = sum(map(len, self.toks)) / len(self.toks)
        self.tf = [Counter(t) for t in self.toks]
        df = Counter(w for t in self.toks for w in set(t))
        n = len(docs)
        self.idf = {w: math.log(1 + (n - c + 0.5) / (c + 0.5)) for w, c in df.items()}

    def top(self, query: str, k: int) -> list[int]:
        q = WORDS.findall(query.lower())
        sc = []
        for i, tf in enumerate(self.tf):
            L, s = len(self.toks[i]), 0.0
            for w in q:
                f = tf.get(w, 0)
                if f:
                    s += self.idf.get(w, 0) * f * (self.k1 + 1) / (f + self.k1 * (1 - self.b + self.b * L / self.avg))
            sc.append(s)
        return sorted(range(len(sc)), key=lambda i: -sc[i])[:k]


PARA = {"badge code": "the number on the staff pass", "launch date": "the day it goes live", "budget code": "the finance line it is charged to",
        "lead engineer": "the engineer in charge", "primary vendor": "the main supplier", "ticket number": "the tracking request id",
        "server rack": "the cabinet housing its computers", "cost centre": "the accounting unit that pays for it",
        "safety officer": "the person responsible for hazards", "test lab": "the facility where it is trialled",
        "firmware build": "the software version on its devices", "contract number": "the agreement reference",
        "site code": "the location identifier", "radio channel": "the frequency its crews talk on",
        "review board": "the panel that approves changes", "insurance policy": "the coverage reference",
        "patent filing": "the intellectual property application", "shipping lane": "the route its freight takes",
        "warehouse bay": "the storage slot for its parts", "audit reference": "the inspection record id",
        "grant number": "the funding award id", "license key": "the activation string", "cable batch": "the wiring lot",
        "paint colour code": "the finish shade", "tool serial": "the instrument's manufacturing id",
        "vehicle plate": "the registration of its truck", "gate pass": "the entry permit", "sensor model": "the detector type",
        "backup tape": "the archive cartridge label", "escrow account": "the held-funds account",
        "customs form": "the border declaration", "survey plot": "the land parcel"}
assert set(PARA) == set(ATTRS)

PREAMBLE = ("You are a records assistant for a large engineering programme. Answer using only the documents provided. "
            "Reply with the exact value only, no explanation. If a value is not in the documents, reply NOT FOUND.")


def make_sessions(facts, n_sessions: int, n_turns: int, seed: int = 1, n_projects: int | None = None, kind: str = "lookup"):
    """kind: lookup = exact attribute name; para = the attribute described in other words (BM25 cannot match it);
    cross = a question about two projects (two answers); drift = the first half of a session is about its own project, the second half
    about another one. n_projects < n_sessions makes agents share projects. Turns beyond the 32 attributes repeat attributes."""
    rng = random.Random(seed)
    projects = sorted({p for p, _ in facts})
    rng.shuffle(projects)
    pool = projects[:n_projects or n_sessions]
    out = []
    for i in range(n_sessions):
        p = pool[i % len(pool)]
        other = rng.choice([x for x in projects if x != p])
        attrs = [a for _ in range(math.ceil(n_turns / len(ATTRS))) for a in rng.sample(ATTRS, len(ATTRS))][:n_turns]
        turns = []
        for t, a in enumerate(attrs):
            if kind == "lookup":
                turns.append({"q": f"What is the {a} for Project {p}?", "attr": a, "projects": [p], "answers": [facts[(p, a)]]})
            elif kind == "para":
                turns.append({"q": f"For Project {p}, what is {PARA[a]}?", "attr": a, "projects": [p], "answers": [facts[(p, a)]]})
            elif kind == "cross":
                turns.append({"q": f"What is the {a} for Project {p}, and what is it for Project {other}?", "attr": a, "projects": [p, other],
                              "answers": [facts[(p, a)], facts[(other, a)]]})
            elif kind == "drift":
                q_p = p if t < n_turns // 2 else other
                turns.append({"q": f"What is the {a} for Project {q_p}?", "attr": a, "projects": [q_p], "answers": [facts[(q_p, a)]]})
            else:
                raise ValueError(kind)
            turns[-1]["answer"] = turns[-1]["answers"][0]
        out.append({"project": p, "turns": turns})
    return out


def context_text(chunks_subset) -> str:
    return "\n\n".join(c["text"] for c in chunks_subset)


def build_messages(arm: str, chunks, bm, session, turn, K_hybrid=24, K_rag=5, salt: str = ""):
    q = session["turns"][turn]["q"]
    if arm == "full":
        ctx = context_text(chunks)
    elif arm == "rag":
        ctx = context_text([chunks[i] for i in bm.top(q, K_rag)])
    elif arm.startswith("hybrid"):
        k = int(arm[6:]) if arm[6:] else K_hybrid  # "hybrid" = 24 chunks, "hybrid10" = the 8 project chunks plus 2
        idx = sorted(bm.top(f"Project {session['project']}", k))  # stable: same set, same order, every turn
        ctx = context_text([chunks[i] for i in idx])
    elif arm.startswith("adaptive"):
        # hybrid keyed on the QUESTION's project: one project -> its cached working set; two -> retrieve for the question; none -> RAG
        k = int(arm[8:]) if arm[8:] else K_hybrid
        ps = session["turns"][turn].get("projects", [session["project"]])
        if len(ps) == 1:
            idx = sorted(bm.top(f"Project {ps[0]}", k))
        else:
            idx = sorted(bm.top(q, 2 * K_rag))
        ctx = context_text([chunks[i] for i in idx])
    else:
        raise ValueError(arm)
    # salt: a run id at the very start of the prompt, so no prefix-cache block is shared between arms (every block hash depends on all
    # tokens before it); agents inside one arm all carry the same salt and still share. Without it, an arm that runs second reuses what
    # the first arm cached and looks faster than it is.
    head = f"[run {salt}] " if salt else ""
    return [{"role": "system", "content": head + PREAMBLE + "\n\nDocuments:\n\n" + ctx}, {"role": "user", "content": q}]


def stream_chat(base: str, model: str, messages, max_tokens: int = 40, timeout: int = 900) -> dict:
    body = json.dumps({"model": model, "messages": messages, "max_tokens": max_tokens, "temperature": 0, "stream": True,
                       "stream_options": {"include_usage": True}}).encode()
    req = urllib.request.Request(base + "/v1/chat/completions", body, {"Content-Type": "application/json"})
    t0, ttft, text, usage = time.perf_counter(), None, "", {}
    with urllib.request.urlopen(req, timeout=timeout) as r:
        for raw in r:
            line = raw.decode().strip()
            if not line.startswith("data:") or line == "data: [DONE]":
                continue
            d = json.loads(line[5:])
            if d.get("usage"):
                usage = d["usage"]
            for ch in d.get("choices", []):
                piece = (ch.get("delta") or {}).get("content") or ""
                if piece and ttft is None:
                    ttft = time.perf_counter() - t0
                text += piece
    total = time.perf_counter() - t0
    cached = ((usage.get("prompt_tokens_details") or {}).get("cached_tokens")) or 0
    return {"ttft": ttft if ttft is not None else total, "total": total, "text": text.strip(),
            "prompt_tokens": usage.get("prompt_tokens", 0), "cached_tokens": cached}


def run_arm(base, model, arm, chunks, bm, sessions, n_turns, builder=None, **kw) -> dict:
    """All sessions run at once (one thread each); a session's turns are sequential. `builder(arm, chunks, bm, session, turn, **kw)`
    replaces build_messages for other corpora; a turn with a "letter" is multiple choice and is right when the reply starts with it."""
    rows, lock = [], threading.Lock()

    def one(s):
        for t in range(min(n_turns, len(s["turns"]))):
            r = stream_chat(base, model, (builder or build_messages)(arm, chunks, bm, s, t, **kw))
            turn = s["turns"][t]
            if "letter" in turn:
                m = re.match(r"\s*\(?([A-D])\b", r["text"].upper())
                ok = bool(m) and m.group(1) == turn["letter"]
            else:
                ok = all(a.lower() in r["text"].lower() for a in (turn.get("answers") or [turn["answer"]]))
            r.update(turn=t, ok=ok, project=s["project"])
            with lock:
                rows.append(r)

    t0 = time.perf_counter()
    with ThreadPoolExecutor(len(sessions)) as ex:
        list(ex.map(one, sessions))
    return {"wall_s": time.perf_counter() - t0, "rows": rows}


def _pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))] if xs else None


def summarize(res: dict, gpu_usd_per_hour: float = 3.95) -> dict:
    rows, wall = res["rows"], res["wall_s"]
    n = len(rows)
    ok = sum(r["ok"] for r in rows)
    first = [r["ttft"] for r in rows if r["turn"] == 0]
    later = [r["ttft"] for r in rows if r["turn"] > 0]
    pt = sum(r["prompt_tokens"] for r in rows)
    ct = sum(r["cached_tokens"] for r in rows)
    return {"n": n, "acc": round(ok / n, 4), "ttft_turn1_p50": _r(_pct(first, 0.5)), "ttft_later_p50": _r(_pct(later, 0.5)),
            "ttft_all_p50": _r(_pct([r["ttft"] for r in rows], 0.5)), "ttft_all_p95": _r(_pct([r["ttft"] for r in rows], 0.95)),
            "latency_p50": _r(_pct([r["total"] for r in rows], 0.5)), "turns_per_min": round(n / wall * 60, 1),
            "wall_s": round(wall, 1), "prompt_tokens_mean": round(pt / n), "cached_frac": round(ct / pt, 3) if pt else 0.0,
            "gpu_s_per_correct": round(wall / ok, 2) if ok else None,
            "usd_per_1k_correct": round(wall * gpu_usd_per_hour / 3600 / ok * 1000, 3) if ok else None}


def _r(x):
    return None if x is None else round(x, 3)
