"""The traits the measures know about: which classifier, what to ask the pairwise judge, and held-out gold pairs."""
from __future__ import annotations

from dataclasses import dataclass, field

from .gold import GOLD as SENTIMENT_GOLD
from .gold_formality import GOLD as FORMALITY_GOLD


@dataclass(frozen=True)
class Trait:
    name: str
    classifier: str
    positive_label: str            # the classifier label for "more of the trait"
    negative_label: str
    question: str                  # what the pairwise judge is asked
    answer_prefix: str             # the text right before the answer token
    examples: tuple[tuple[str, str, str], ...]   # (text A, text B, "A" or "B" = which has more of the trait)
    gold: tuple[tuple[str, str], ...] = field(default=())   # (more of the trait, less of the trait)


SENTIMENT = Trait(
    name="sentiment",
    classifier="siebert/sentiment-roberta-large-english",
    positive_label="positive",
    negative_label="negative",
    question="Decide which one is more positive in sentiment (happier, more favourable, more approving).",
    answer_prefix="More positive:",
    examples=(
        ("The service was slow and the food was cold.", "The staff were lovely and the meal was great.", "B"),
        ("I am thrilled with how the day went.", "Nothing about this trip worked out.", "A"),
    ),
    gold=tuple(SENTIMENT_GOLD),
)

FORMALITY = Trait(
    name="formality",
    classifier="s-nlp/roberta-base-formality-ranker",
    positive_label="formal",
    negative_label="informal",
    question="Decide which one is more formal in register (polite, professional, precise; no slang, no contractions).",
    answer_prefix="More formal:",
    examples=(
        ("hey so yeah we gotta fix that thing asap lol", "We must resolve this matter as soon as possible.", "B"),
        ("I would be grateful if you could confirm the schedule.", "can u tell me when its happening", "A"),
    ),
    gold=tuple(FORMALITY_GOLD),
)

TRAITS = {t.name: t for t in (SENTIMENT, FORMALITY)}
