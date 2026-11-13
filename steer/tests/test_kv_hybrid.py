from playbox_steer import kv_hybrid as K

count = lambda s: max(1, len(s) // 4)


def _setup():
    chunks, facts = K.gen_corpus(20000, count, seed=0)
    return chunks, facts, K.BM25([c["text"] for c in chunks])


def test_corpus_shape_and_facts():
    chunks, facts, _ = _setup()
    assert len(chunks) % 8 == 0 and len({c["project"] for c in chunks}) == len(chunks) // 8
    assert len(facts) == 32 * (len(chunks) // 8)
    c = chunks[0]
    assert any(f"is {v}." in c["text"] for (p, a), v in facts.items() if p == c["project"])


def test_rag_finds_the_fact_and_hybrid_covers_the_project():
    chunks, facts, bm = _setup()
    sess = K.make_sessions(facts, 8, 4)
    hit = 0
    for s in sess:
        for t in s["turns"]:
            ctx = K.context_text([chunks[i] for i in bm.top(t["q"], 5)])
            hit += t["answer"] in ctx
        mine = {c["id"] for c in chunks if c["project"] == s["project"]}
        assert mine <= set(bm.top(f"Project {s['project']}", 24))
    assert hit / 32 >= 0.9


def test_hybrid_prefix_is_stable_across_turns_and_rag_is_not():
    chunks, facts, bm = _setup()
    s = K.make_sessions(facts, 1, 3)[0]
    h = [K.build_messages("hybrid", chunks, bm, s, t)[0]["content"] for t in range(3)]
    r = [K.build_messages("rag", chunks, bm, s, t)[0]["content"] for t in range(3)]
    assert h[0] == h[1] == h[2]
    assert len(set(r)) > 1


def test_summarize():
    rows = [{"ok": True, "ttft": 0.5, "total": 1, "turn": 0, "prompt_tokens": 100, "cached_tokens": 0},
            {"ok": False, "ttft": 0.1, "total": 0.5, "turn": 1, "prompt_tokens": 100, "cached_tokens": 90}]
    s = K.summarize({"rows": rows, "wall_s": 6.0})
    assert s["acc"] == 0.5 and s["turns_per_min"] == 20.0 and s["cached_frac"] == 0.45 and s["ttft_later_p50"] == 0.1
