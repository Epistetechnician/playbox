"""Swarm primitives: agent assignment, the novelty filter, aggregation, trial scoring, the paired sign test."""
import random

import pytest
import torch

from playbox_steer import telepathy_swarm as S

LEX = {"ocean": {"sea", "ocean"}, "space": {"star", "orbit"}, "music": {"song", "band"}}


def test_assign_agents_shape():
    for seed in range(20):
        true, agents = S.assign_agents(list("abcdefgh"), 3, 6, random.Random(seed))
        assert len(set(true)) == 3 and len(agents) == 6
        assert agents.count(true[0]) == 4 and agents.count(true[1]) == 1 and agents.count(true[2]) == 1
    with pytest.raises(ValueError):
        S.assign_agents(list("abc"), 3, 2, random.Random(0))


def test_novelty_select_takes_one_agent_per_distinct_direction():
    g = torch.Generator().manual_seed(0)
    base = torch.randn(3, 64, generator=g)
    # four noisy copies of direction 0, one of direction 1, one of direction 2
    vecs = torch.stack([base[0] + 0.05 * torch.randn(64, generator=g) for _ in range(4)] + [base[1], base[2]])
    chosen = S.novelty_select(vecs, 3)
    assert sorted(chosen) == sorted({4, 5, next(i for i in chosen if i < 4)})  # both singles plus one of the copies
    assert len(set(chosen)) == 3


def test_novelty_select_is_deterministic_and_validates():
    v = torch.randn(5, 16, generator=torch.Generator().manual_seed(1))
    assert S.novelty_select(v, 2) == S.novelty_select(v, 2)
    with pytest.raises(ValueError):
        S.novelty_select(v, 6)


def test_aggregate_norm_and_scale():
    v = torch.randn(4, 32, generator=torch.Generator().manual_seed(2)) * torch.tensor([1.0, 10.0, 0.1, 5.0])[:, None]
    a = S.aggregate(v, coef=0.75, norm_b=20.0)
    assert a.norm().item() == pytest.approx(15.0, rel=1e-5)
    assert S.aggregate(v, 0.75, 20.0, scale=2.0).norm().item() == pytest.approx(30.0, rel=1e-5)
    # scale-free in the inputs: every agent counts the same however big its raw vector was
    assert torch.allclose(a, S.aggregate(v * torch.tensor([3.0, 0.2, 40.0, 1.0])[:, None], 0.75, 20.0), atol=1e-5)


def test_score_trial():
    texts = ["the sea and the ocean at night", "a star in orbit", "nothing here"]
    r = S.score_trial(texts, LEX, ["ocean", "space"])
    assert r["hits"] == {"ocean": 2, "space": 2, "music": 0}
    assert set(r["pred"]) == {"ocean", "space"} and r["recall"] == 1.0 and r["full"] and r["precision"] == 1.0
    r = S.score_trial(["a song by the band, a song"], LEX, ["ocean", "space"])
    assert r["pred"] == ["music"] and r["recall"] == 0.0 and not r["full"] and r["precision"] == 0.0
    r = S.score_trial(["no topic words"], LEX, ["ocean"])
    assert r["pred"] == [] and r["recall"] == 0.0 and r["precision"] == 0.0


def test_score_trial_only_top_k_count():
    texts = ["sea sea sea star star song"]
    r = S.score_trial(texts, LEX, ["ocean"])  # k = 1: only the top concept is predicted
    assert r["pred"] == ["ocean"] and r["recall"] == 1.0
    r = S.score_trial(texts, LEX, ["music"])
    assert r["pred"] == ["ocean"] and r["recall"] == 0.0


def test_paired_sign_p():
    assert S.paired_sign_p([1, 1, 1, 1], [0, 0, 0, 0]) == (4, 0, pytest.approx(0.125))
    assert S.paired_sign_p([1, 0], [0, 1]) == (1, 1, 1.0)
    assert S.paired_sign_p([1, 1], [1, 1]) == (0, 0, 1.0)
