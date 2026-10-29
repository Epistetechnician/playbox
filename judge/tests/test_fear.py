"""The fear measures against held-out gold pairs (loads real models from the Hugging Face cache)."""
import pytest

from playbox_judge.fear import FEAR, GOLD
from playbox_judge.measures import PairwiseJudge, TraitClassifier


def test_the_classifier_ranks_fear_gold_pairs():
    clf = TraitClassifier(FEAR)
    more, less = clf.score([a for a, _ in GOLD]), clf.score([b for _, b in GOLD])
    assert sum(x > y for x, y in zip(more, less)) / len(GOLD) >= 0.95


def test_the_judge_ranks_fear_gold_pairs():
    d = PairwiseJudge(FEAR).log_odds_first_more_positive(GOLD)
    assert sum(x > 0 for x in d) / len(GOLD) >= 0.9
