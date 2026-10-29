"""Two behavioral measures of sentiment, neither of which is the steering direction.

- `SentimentClassifier`: a trained classifier (`siebert/sentiment-roberta-large-english`). Score = the logit
  difference between its positive and negative classes: higher is more positive.
- `PairwiseJudge`: a local language model asked which of two texts is more positive, in both orders so a
  position bias cancels. Score(x, y) = the log-odds that x is more positive than y. This is the paired form of
  the pairwise-comparison idea in lyramakesmusic/cardinal-harness, without its latent-score fit: E1 already pairs
  each steered text with the clean text for the same seed, so a fit across items is not needed.
"""
from __future__ import annotations

import math

import torch
from transformers import AutoModelForCausalLM, AutoModelForSequenceClassification, AutoTokenizer

from .traits import SENTIMENT, Trait

MAX_CHARS = 400
CLASSIFIER = SENTIMENT.classifier
JUDGE = "Qwen/Qwen3-1.7B-Base"


def device() -> str:
    return "mps" if torch.backends.mps.is_available() else "cpu"


def _clip(t: str) -> str:
    t = " ".join(t.split())
    return t[:MAX_CHARS] if t else "."


class TraitClassifier:
    """A trained classifier for `trait`. Score = logit(positive label) - logit(negative label): higher is more of
    the trait."""

    def __init__(self, trait: Trait = SENTIMENT, batch: int = 32):
        self.tok = AutoTokenizer.from_pretrained(trait.classifier)
        self.model = AutoModelForSequenceClassification.from_pretrained(trait.classifier).eval().to(device())
        labels = {v.lower(): k for k, v in self.model.config.id2label.items()}
        self.pos, self.neg = labels[trait.positive_label], labels[trait.negative_label]
        self.batch = batch

    @torch.no_grad()
    def score(self, texts: list[str]) -> list[float]:
        out: list[float] = []
        for i in range(0, len(texts), self.batch):
            enc = self.tok([_clip(t) for t in texts[i : i + self.batch]], return_tensors="pt", padding=True, truncation=True, max_length=128).to(self.model.device)
            logits = self.model(**enc).logits.float()
            out += (logits[:, self.pos] - logits[:, self.neg]).cpu().tolist()
        return out


def prompt_for(trait: Trait) -> str:
    """The judge's prompt with `{a}` and `{b}` left to fill; braces in the trait's own text are escaped."""
    esc = lambda t: t.replace("{", "{{").replace("}", "}}")  # noqa: E731
    shots = "".join(f"Text A: {esc(a)}\nText B: {esc(b)}\n{trait.answer_prefix} {ans}\n\n" for a, b, ans in trait.examples)
    return f"Two short texts are shown. {trait.question}\n\n{shots}Text A: {{a}}\nText B: {{b}}\n{trait.answer_prefix}"


class PairwiseJudge:
    def __init__(self, trait: Trait = SENTIMENT, name: str = JUDGE, batch: int = 8):
        self.prompt = prompt_for(trait)
        self.tok = AutoTokenizer.from_pretrained(name)
        self.tok.padding_side = "left"
        if self.tok.pad_token is None:
            self.tok.pad_token = self.tok.eos_token
        self.model = AutoModelForCausalLM.from_pretrained(name, torch_dtype=torch.float32, attn_implementation="eager").eval().to(device())
        self.id_a = self.tok.encode(" A", add_special_tokens=False)
        self.id_b = self.tok.encode(" B", add_special_tokens=False)
        if len(self.id_a) != 1 or len(self.id_b) != 1:
            raise RuntimeError("the judge's tokenizer does not give single tokens for ' A' and ' B'")
        self.id_a, self.id_b = self.id_a[0], self.id_b[0]
        self.batch = batch

    @torch.no_grad()
    def _a_minus_b(self, pairs: list[tuple[str, str]]) -> list[float]:
        out: list[float] = []
        for i in range(0, len(pairs), self.batch):
            prompts = [self.prompt.format(a=_clip(a), b=_clip(b)) for a, b in pairs[i : i + self.batch]]
            enc = self.tok(prompts, return_tensors="pt", padding=True).to(self.model.device)
            logp = torch.log_softmax(self.model(**enc).logits[:, -1].float(), -1)
            diff = (logp[:, self.id_a] - logp[:, self.id_b]).cpu().tolist()
            # Left padding with the default attention gave NaN on MPS; a NaN would compare false everywhere and
            # read as "no preference", so refuse it loudly.
            if not all(math.isfinite(v) for v in diff):
                raise RuntimeError("the judge produced a non-finite score")
            out += diff
        return out

    def log_odds_first_more_positive(self, pairs: list[tuple[str, str]]) -> list[float]:
        """For each (x, y): half the difference between asking with x first and with y first."""
        fwd = self._a_minus_b(pairs)
        rev = self._a_minus_b([(y, x) for x, y in pairs])
        return [(f - r) / 2 for f, r in zip(fwd, rev)]


SentimentClassifier = TraitClassifier  # the original name; sentiment is the default trait
