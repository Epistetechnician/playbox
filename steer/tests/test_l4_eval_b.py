import random

from playbox_steer import l4_eval_b as E


def rows(gap, n_topics=8, per=10, seed=0):
    """none = 4, own = 1; other_own recovers 0.5 of the gain, mapped recovers 0.5 + gap."""
    r = random.Random(seed)
    out = []
    for t in range(n_topics):
        for _ in range(per):
            j = r.uniform(-0.1, 0.1)
            out.append({"topic": f"t{t}", "none": 4.0, "own": 1.0, "other_own": 4 - 3 * 0.5 + j, "mapped": 4 - 3 * (0.5 + gap) + j, "mapped_wrong": 4.0})
    return out


def test_retention_and_difference():
    rs = rows(0.25)
    assert abs(E.retention(rs, "own") - 1.0) < 1e-9
    assert abs(E.diff(rs, "mapped", "other_own") - 0.25) < 1e-9
    assert E.retention(rs, "mapped_wrong") == 0


def test_cluster_bootstrap_brackets_the_truth_and_respects_clusters():
    rs = rows(0.25)
    b = E.cluster_bootstrap(rs, "mapped", "other_own", n_boot=500)
    assert b["n_topics"] == 8 and b["n_items"] == 80
    assert b["ci95"][0] <= b["point"] <= b["ci95"][1] and abs(b["point"] - 0.25) < 1e-9
    # a gap that depends on the topic gives a wider interval than the same gap spread evenly
    het = [dict(r, mapped=r["mapped"] - (0.3 if r["topic"] in ("t0", "t1", "t2", "t3") else -0.3) * 3) for r in rs]
    assert E.cluster_bootstrap(het, "mapped", "other_own", n_boot=500)["ci95"][1] - E.cluster_bootstrap(het, "mapped", "other_own", n_boot=500)["ci95"][0] > \
        b["ci95"][1] - b["ci95"][0]


def test_per_topic_has_every_topic():
    assert len(E.per_topic(rows(0.1), "mapped", "other_own")) == 8
