import copy

import pytest

from playbox_judge.e3 import merge_shards, summarize


def shard(arms, **over):
    base = {"coef": 0.3, "tokens": 4, "site": "resid_post", "layers": [6, 12], "prompts": ["a", "b"], "fit_chunks": 10,
            "info": {"fit": {"r2_heldout": 0.5}}, "norm_ref": 3.0,
            "arms": {"clean": {"texts": ["x", "y"], "kl": 0.0}, **{n: {"texts": [n, n], "kl": 0.1} for n in arms}}}
    base.update(over)
    return base


def test_merge_combines_disjoint_arms_and_checks_the_shared_ones():
    m = merge_shards([shard(["direct", "mapped_null0"]), shard(["mapped", "random_null0"])])
    assert set(m["arms"]) == {"clean", "direct", "mapped", "mapped_null0", "random_null0"}
    with pytest.raises(ValueError, match="two shards ran"):
        merge_shards([shard(["direct"]), shard(["direct"])])
    with pytest.raises(ValueError, match="disagree on coef"):
        merge_shards([shard(["direct"]), shard(["mapped"], coef=0.5)])
    bad = shard(["mapped"])
    bad["arms"]["clean"] = {"texts": ["x", "CHANGED"], "kl": 0.0}
    with pytest.raises(ValueError, match="not deterministic"):
        merge_shards([shard(["direct"]), bad])
    other_fit = copy.deepcopy(shard(["mapped"]))
    other_fit["info"]["fit"]["r2_heldout"] = 0.6
    with pytest.raises(ValueError):
        merge_shards([shard(["direct"]), other_fit])


def test_summary_ranks_direct_and_mapped_against_each_null_population_and_reports_retention():
    n = 16
    eff = {"direct": [2.0 + 0.1 * (i % 3) for i in range(n)], "mapped": [1.0 + 0.1 * (i % 3) for i in range(n)]}
    for k in range(6):
        eff[f"mapped_null{k}"] = [0.05 * (k - 3) + 0.01 * (i % 2) for i in range(n)]
        eff[f"random_null{k}"] = [0.03 * (k - 3) for _ in range(n)]
    kl = {"direct": 0.3, "mapped": 0.35, **{f"mapped_null{k}": 0.2 for k in range(6)}, **{f"random_null{k}": 0.1 for k in range(6)}}
    s = summarize(eff, kl)
    assert s["populations"]["mapped_null"]["n"] == 6 and s["populations"]["random_null"]["n"] == 6
    for pop in s["populations"].values():
        assert pop["direct_percentile"] == 1.0 and pop["mapped_percentile"] == 1.0 and pop["mapped_z"] > 3
    assert s["populations"]["mapped_null"]["mean_kl"] == pytest.approx(0.2)
    mean = lambda xs: sum(xs) / len(xs)  # noqa: E731
    assert s["retained"] == pytest.approx(mean(eff["mapped"]) / mean(eff["direct"]), rel=1e-9)
    d = s["mapped_minus_direct"]
    assert d["mean"] == pytest.approx(-1.0) and d["high"] < 0
    assert s["arms"]["mapped"]["kl"] == 0.35


def test_summary_without_nulls_or_pairs_degrades_quietly():
    s = summarize({"direct": [1.0, 2.0, 3.0]}, {})
    assert s["populations"] == {} and "retained" not in s
    assert summarize({"direct": [0.0, 0.0], "mapped": [1.0, 1.0]}, {})["retained"] is None
