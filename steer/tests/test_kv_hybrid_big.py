from playbox_steer import kv_hybrid as K
from playbox_steer import kv_hybrid_big as B

count = lambda s: max(1, len(s) // 4)


def test_big_corpus_working_set_is_the_project():
    chunks, facts = B.gen_corpus_big(30000, count, seed=0)
    assert len(chunks) % 64 == 0 and len(facts) == 256 * (len(chunks) // 64)
    bm = K.BM25([c["text"] for c in chunks])
    ss = B.make_sessions_big(facts, 6, 5, n_projects=2)
    for s in ss:
        top = bm.top(f"Project {s['project']}", 64)
        assert {chunks[i]["project"] for i in top} == {s["project"]}
        for t in s["turns"]:
            ctx = K.build_messages("hybrid64", chunks, bm, s, 0)[0]["content"]
            assert t["answers"][0] in ctx


def _rows():
    arts = [(f"article {i} " + " ".join(f"w{i}_{j}" for j in range(500)), [(f"What is q{n} of {i}?", ["x", "y", "z", "w"], n % 4) for n in range(5)]) for i in range(8)]
    return [{"article": a, "question": q, "options": o, "answer": ans} for a, qs in arts for q, o, ans in qs]


def test_quality_build_and_arms():
    arts = B.group_articles(_rows())
    assert len(arts) == 8 and len(arts[0]["questions"]) == 5
    chunks, sess = B.build_quality(arts, 1500, count, seed=0, max_turns=4)
    assert len(sess) >= 2 and all(len(s["turns"]) == 4 for s in sess)
    bm = K.BM25([c["text"] for c in chunks])
    s = sess[0]
    full = B.quality_messages("full", chunks, bm, s, 0)[0]["content"]
    doc = B.quality_messages("doc", chunks, bm, s, 0)[0]["content"]
    h = [B.quality_messages("hybrid6", chunks, bm, s, t)[0]["content"] for t in range(4)]
    assert len(full) > len(doc) and h[0] == h[1] == h[3]
    assert B.quality_messages("rag3", chunks, bm, s, 0)[1]["content"].endswith("Answer with the letter only.")


def test_multiple_choice_scoring(monkeypatch):
    def fake(base, model, messages, **kw):
        return {"ttft": 0.1, "total": 0.2, "text": "(b) it is", "prompt_tokens": 1, "cached_tokens": 0}
    monkeypatch.setattr(K, "stream_chat", fake)
    ses = [{"project": 0, "turns": [{"q": "q", "letter": "B"}, {"q": "q", "letter": "C"}]}]
    r = K.run_arm("", "m", "full", [], None, ses, 5, builder=lambda *a, **k: [])
    assert [x["ok"] for x in sorted(r["rows"], key=lambda x: x["turn"])] == [True, False]
