"""The measures against held-out gold pairs. These load real models (cached under ~/.cache/huggingface)."""
import pytest

from playbox_judge.gold import GOLD
from playbox_judge.measures import PairwiseJudge, SentimentClassifier
from playbox_judge.stats import bootstrap_ci, percentile_against, ranks, spearman


@pytest.fixture(scope="module")
def clf():
    return SentimentClassifier()


@pytest.fixture(scope="module")
def judge():
    return PairwiseJudge()


def test_classifier_ranks_gold_pairs(clf):
    pos = clf.score([p for p, _ in GOLD])
    neg = clf.score([n for _, n in GOLD])
    acc = sum(a > b for a, b in zip(pos, neg)) / len(GOLD)
    assert acc >= 0.95, acc


def test_judge_ranks_gold_pairs_and_is_antisymmetric(judge):
    d = judge.log_odds_first_more_positive(GOLD)
    acc = sum(x > 0 for x in d) / len(GOLD)
    assert acc >= 0.9, acc
    flipped = judge.log_odds_first_more_positive([(n, p) for p, n in GOLD])
    # Averaging both orders makes the score exactly antisymmetric.
    assert all(abs(a + b) < 1e-4 for a, b in zip(d, flipped))
    same = judge.log_odds_first_more_positive([("It was fine.", "It was fine.")])
    assert abs(same[0]) < 1e-4


def test_stats_helpers():
    assert bootstrap_ci([2.0] * 10) == (2.0, 2.0, 2.0)
    assert percentile_against(3, [1, 2, 3, 4]) == 0.5
    assert ranks([3, 1, 3, 2]) == [3.5, 1.0, 3.5, 2.0]
    assert spearman([1, 2, 3, 4], [10, 20, 30, 40]) == pytest.approx(1.0)
    assert spearman([1, 2, 3, 4], [4, 3, 2, 1]) == pytest.approx(-1.0)


def test_batching_does_not_change_scores(judge):
    pairs = [GOLD[0], ("a", "b"), GOLD[5], ("Short.", "A much longer second text that makes this prompt longer than the others in the batch."), GOLD[9]]
    batched = judge.log_odds_first_more_positive(pairs)
    single = [judge.log_odds_first_more_positive([p])[0] for p in pairs]
    assert all(abs(a - b) < 1e-2 for a, b in zip(batched, single)), (batched, single)
