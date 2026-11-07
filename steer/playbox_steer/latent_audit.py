"""Commit-and-challenge audit of a hub vector (docs/latent-channels.md, sections 5 and 6h).

A sender signs a commitment to the sentences it read along with its vector. An auditor can challenge a message; the sender
reveals the sentences (the host checks the reveal against the signed commitment); the auditor encodes the revealed sentences
through its OWN model and spoke and compares the result with the vector the sender sent. This works across families,
which the E6 recompute audit cannot (that one needs identical weights).

What a pass means: the vector is consistent with the sentences the sender committed to. It does not say the sentences are
harmless (an auditor, or a person, can read them: they are text), and a sender that commits to sentences that really
produce its payload passes. What a fail means: the sender sent a vector that does not follow from what it committed to,
or it never revealed.
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field

import torch

from . import hub as H


# The bounds the host enforces on a reveal (`latent::Reveal::parse`); an auditor holds a reveal to them too, because it spends
# model compute on every sentence and a host that is not honest could serve more.
MAX_REVEAL_BYTES = 16 * 1024
MAX_REVEAL_SENTENCES = 64
MAX_SENTENCE_BYTES = 512


@dataclass
class Verdict:
    ok: bool
    cosine: float | None = None
    reasons: list[str] = field(default_factory=list)


def audit(hub: H.Hub, auditor: str, head: dict, vector: torch.Tensor, reveal: bytes | None, thought_of, threshold: float) -> Verdict:
    """`head`, `vector`: from `hub.unpack_signed`. `reveal`: the sender's reveal bytes (None if it never answered).
    `thought_of(sentences)`: the auditor model's difference-of-means thought for those sentences, in its own space."""
    if not head.get("commit"):
        return Verdict(False, reasons=["the message carries no commitment"])
    if reveal is None:
        return Verdict(False, reasons=["the sender did not reveal"])
    try:
        if len(reveal) > MAX_REVEAL_BYTES:
            raise ValueError("too large")
        rev = json.loads(reveal)
        salt, sentences = rev["salt"], rev["sentences"]
        if (not isinstance(salt, str) or not 16 <= len(salt) <= 64 or any(c not in "0123456789abcdefABCDEF" for c in salt)
                or not isinstance(sentences, list) or not 0 < len(sentences) <= MAX_REVEAL_SENTENCES
                or any(not isinstance(x, str) or "\n" in x or len(x.encode()) > MAX_SENTENCE_BYTES for x in sentences)):
            raise ValueError("out of bounds")
        matches = H.make_commit(salt, sentences) == head["commit"]
    except (ValueError, KeyError, TypeError, RecursionError):
        return Verdict(False, reasons=["malformed reveal"])
    if not matches:
        return Verdict(False, reasons=["the reveal does not match the commitment"])
    mine = hub.encode(auditor, thought_of(sentences))
    cos = float(torch.nn.functional.cosine_similarity(mine, vector, dim=0))
    if cos < threshold:
        return Verdict(False, cos, [f"the vector does not follow from the revealed sentences (cosine {cos:.2f} < {threshold:.2f})"])
    return Verdict(True, cos)
