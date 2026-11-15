import torch

from playbox_steer import metered_bench as M
from playbox_steer.metered_hf import HFIO
from playbox_steer.telepathy import lexicon_id

CONCEPTS = {f"c{i}": {"sentences": [f"sentence {i} number {j} about topic{i}" for j in range(8)], "lexicon": [f"topic{i}"]} for i in range(5)}
count = lambda s: max(1, len(s) // 4)


def test_world_shape_and_answers():
    chunks, sessions = M.build_world(10, count, CONCEPTS, fact_turns=3)
    assert len(sessions) == 10 and len({s["project"] for s in sessions}) == 10
    assert len([c for c in chunks if "overview" in c["text"]]) == 10 and len(chunks) == 10 * 9
    for i, s in enumerate(sessions):
        assert s["concept"] == f"c{i % 5}"
        assert [t["kind"] for t in s["turns"]] == ["topic", "fact", "fact", "fact"]
        for t in s["turns"][1:]:
            assert any(t["answer"] in c["text"] for c in chunks if c["project"] == s["project"])


def test_meter_and_summary_amortise_only_latent():
    m = M.Meter()
    for arm, secs in (("text_rag", 2.0), ("latent_free", 1.0)):
        with m.track(arm, "gen"):
            pass
        m.t[(arm, "gen")] = secs
    row = lambda ok_t, ok_f: {"rows": [{"kind": "topic", "ok": ok_t, "ctx_tokens": 10}] * 2 + [{"kind": "fact", "ok": ok_f, "ctx_tokens": 30}] * 2}
    res = {"text_rag": row(False, True), "latent_free": row(True, False)}
    s = M.summarize(res, m, n_sessions=2, rate_usd_h=3600.0, fit_s=100.0, amortise_sessions=1000)
    assert s["text_rag"]["fit_amortised_s"] == 0 and s["latent_free"]["fit_amortised_s"] == 0.2
    assert s["text_rag"]["gpu_s_total"] == 2.0 and s["latent_free"]["gpu_s_total"] == 1.2
    assert s["text_rag"]["correct_total"] == 2 and s["text_rag"]["topic"]["acc"] == 0 and s["text_rag"]["fact"]["acc"] == 1
    assert s["text_rag"]["correct_per_usd"] == 2 / (2.0 * 3600 / 3600)  # 3600 USD/h -> 1 USD per GPU-second
    lo, hi = s["text_rag"]["fact"]["ci95"]
    assert 0 < lo < 1 and hi == 1.0 or hi < 1.0


def test_wilson():
    assert M.wilson(0, 0) == (0.0, 0.0)
    lo, hi = M.wilson(8, 10)
    assert 0.4 < lo < 0.5 and 0.9 < hi < 1.0


class Stub:
    """Records what each call received; answers a fact turn with its own answer when it was given the cache."""

    def __init__(self):
        self.calls = []

    def ids(self, text, special=True):
        return torch.zeros(1, max(1, len(text) // 8), dtype=torch.long)

    def prefill(self, ids):
        self.calls.append("prefill")
        return object()

    def generate(self, ids, n, cache=None, vec=None, prefix_ids=None):
        self.calls.append(("gen", cache is not None, vec is not None))
        return "topic0" if vec is not None else "nothing"


class FakeLatent:
    free_prompts = ["p1", "p2"]

    def vector(self, concept):
        return torch.ones(2)


def test_channels_only_where_they_apply():
    chunks, sessions = M.build_world(2, count, CONCEPTS, fact_turns=2)
    from playbox_steer.kv_hybrid import BM25
    bm = BM25([c["text"] for c in chunks])
    seen = {}
    for arm in ("hybrid", "latent_free", "latent_q", "swarm", "text_word"):
        io, meter = Stub(), M.Meter()
        M.run_arm(arm, io, chunks, bm, sessions, CONCEPTS, meter, latent=FakeLatent(), lexicon_id=lexicon_id)
        seen[arm] = io.calls
    gens = lambda a: [c for c in seen[a] if c != "prefill"]
    assert seen["hybrid"].count("prefill") == 2 and all(c[1] for c in gens("hybrid"))  # one prefill per session, cache on every turn
    for arm in ("latent_free", "latent_q"):
        assert "prefill" not in seen[arm] and [c[2] for c in gens(arm)] == [True, False, False] * 2  # vector on the topic turn only
    assert seen["swarm"].count("prefill") == 2 and [(c[1], c[2]) for c in gens("swarm")] == [(False, True), (True, False), (True, False)] * 2
    assert "prefill" not in seen["text_word"]


def test_cached_prefix_gives_the_same_tokens_and_survives_reuse(llama_dir):
    from playbox_steer import telepathy as T
    model, tok = T.load(str(llama_dir))
    io = HFIO(model, tok, 1)
    pref, suf1, suf2 = io.ids("the quick brown fox jumps over the lazy dog and keeps running"), io.ids(" one two three", special=False), io.ids(" four five", special=False)
    cache = io.prefill(pref)
    base = lambda suf: io.generate(torch.cat([pref, suf], 1), 6)
    for suf in (suf1, suf2, suf1):  # reuse after cropping must not drift
        assert io.generate(suf, 6, cache=cache, prefix_ids=pref) == base(suf)
    assert cache.get_seq_length() == pref.shape[1]
