"""Two more workloads for the retrieval-then-cache benchmark (playbox_steer/kv_hybrid.py): much larger working sets, and public data.

big   (synthetic) 10 projects x 64 chunks (about 13K tokens each); a project has 256 facts ("the badge code alpha is ..."), so a session on a
      project can ask about any of them. hybrid K=64 caches the whole project as the working set; rag takes the top 5 chunks per question.
quality (public) QuALITY articles (Pang et al., multiple choice, about 5K tokens each, about 9 questions each; HF `emozilla/quality`). A corpus
      is about 22 articles, 128K tokens; a session is one article's questions. Arms: full (all articles), rag5 / ragK (BM25 over chunks, on the
      question and its options), hybridK (K chunks retrieved once from the session's FIRST question, kept as the prefix), doc (the session's
      own article: the oracle working set a product would get when a session is opened on a document)."""
from __future__ import annotations

import random
from collections import OrderedDict

from . import kv_hybrid as K

TAGS = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"]
BIG_ATTRS = [f"{a} {t}" for a in K.ATTRS for t in TAGS]  # 256


def gen_corpus_big(target_tokens: int, count, seed: int = 0, chunks_per_project: int = 64, facts_per_chunk: int = 4):
    rng = random.Random(seed)
    names = [f"{s}-{n}" for s in K.STARS for n in K.NUMS]
    rng.shuffle(names)
    chunks, facts, total, i, pr = [], {}, 0, 0, 0
    while total < target_tokens and pr < len(names):
        name = names[pr]
        order = list(range(len(BIG_ATTRS)))
        rng.shuffle(order)
        for k in range(chunks_per_project):
            lines = []
            for a in order[k * facts_per_chunk:(k + 1) * facts_per_chunk]:
                v = K._value(rng)
                facts[(name, BIG_ATTRS[a])] = v
                lines.append(f"For Project {name}, the {BIG_ATTRS[a]} is {v}.")
            for _ in range(5):
                lines.insert(rng.randint(0, len(lines)), rng.choice(K.FILLER).format(n=rng.choice(K.NOUNS)))
            text = f"[Document {i:04d}] Project {name}, section {k + 1}.\n" + " ".join(lines)
            chunks.append({"id": i, "project": name, "text": text})
            total += count(text)
            i += 1
        pr += 1
    return chunks, facts


def make_sessions_big(facts, n_sessions: int, n_turns: int, n_projects: int, seed: int = 1):
    rng = random.Random(seed)
    projects = sorted({p for p, _ in facts})
    pool = projects[:n_projects]
    out = []
    for i in range(n_sessions):
        p = pool[i % len(pool)]
        turns = []
        for a in rng.sample(BIG_ATTRS, n_turns):
            turns.append({"q": f"What is the {a} for Project {p}?", "attr": a, "projects": [p], "answers": [facts[(p, a)]], "answer": facts[(p, a)]})
        out.append({"project": p, "turns": turns})
    return out


# ---- QuALITY

QPREAMBLE = ("You answer multiple-choice questions about articles. Use only the articles provided. "
             "Reply with the letter of the correct option only (A, B, C or D).")


def group_articles(rows) -> list[dict]:
    """rows: dicts with article, question, options, answer (0-based index). Returns articles with their questions, in first-seen order."""
    g: "OrderedDict[str, list]" = OrderedDict()
    for r in rows:
        g.setdefault(r["article"], []).append(r)
    return [{"text": a, "questions": [{"q": r["question"], "options": list(r["options"]), "answer": int(r["answer"])} for r in rs]}
            for a, rs in g.items()]


def chunk_words(text: str, words: int = 170) -> list[str]:
    w = text.split()
    return [" ".join(w[i:i + words]) for i in range(0, len(w), words)]


def build_quality(articles, target_tokens: int, count, seed: int = 0, max_turns: int = 8):
    rng = random.Random(seed)
    order = list(range(len(articles)))
    rng.shuffle(order)
    chunks, sessions, total = [], [], 0
    for ai in order:
        if total >= target_tokens:
            break
        a, k = articles[ai], len(sessions)
        for j, c in enumerate(chunk_words(a["text"])):
            t = f"[Article {k}, part {j + 1}]\n{c}"
            chunks.append({"id": len(chunks), "project": k, "text": t})
            total += count(t)
        turns = []
        for q in a["questions"][:max_turns]:
            opts = "\n".join(f"{'ABCD'[i]}) {o}" for i, o in enumerate(q["options"]))
            turns.append({"q": f"{q['q']}\n{opts}\nAnswer with the letter only.", "query": q["q"] + " " + " ".join(q["options"]),
                          "letter": "ABCD"[q["answer"]], "projects": [k]})
        sessions.append({"project": k, "turns": turns})
    return chunks, sessions


def quality_messages(arm: str, chunks, bm, session, turn, K_hybrid=24, salt: str = ""):
    t = session["turns"][turn]
    if arm == "full":
        idx = range(len(chunks))
    elif arm.startswith("rag"):
        idx = sorted(bm.top(t["query"], int(arm[3:] or 5)))
    elif arm.startswith("hybrid"):  # retrieved once, from the first question, then fixed for the whole session
        idx = sorted(bm.top(session["turns"][0]["query"], int(arm[6:] or K_hybrid)))
    elif arm == "doc":
        idx = [c["id"] for c in chunks if c["project"] == session["project"]]
    else:
        raise ValueError(arm)
    head = f"[run {salt}] " if salt else ""
    ctx = "\n\n".join(chunks[i]["text"] for i in idx)
    return [{"role": "system", "content": head + QPREAMBLE + "\n\nArticles:\n\n" + ctx}, {"role": "user", "content": t["q"]}]
