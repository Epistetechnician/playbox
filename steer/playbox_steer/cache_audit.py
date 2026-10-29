"""The defence E6 calls for: a signed commitment that binds a KV-cache to the text it claims to come from, and an
audit that recomputes the cache from the text and compares.

A latent handoff sends a cache. Text-level checks (the admission broker, taint tracking, receipts of text) see
nothing in it. So the sender also signs a record `playbox.cache-handoff.v1`: the weights it ran, the sha256 of the
context text, and the sha256 of the exact cache bytes it is sending. The receiver checks, in order:

  1. the signature, and that the weights hash is the one it expects;
  2. that the cache it received hashes to the cache digest in the record (the record is about *this* cache);
  3. the audit: read the text itself with the same model and compare the result to the received cache, entry by
     entry, within a tolerance. A cache that does not come from this text (steered, swapped, edited) fails here.

Step 3 costs one prefill, which is what a latent handoff exists to avoid, so it is an audit, not a gate for every
message: audit a random fraction `p` of handoffs (`detection_probability`). A sender who cheats `n` times is caught
with probability `1 - (1 - p)^n`. Step 3 needs the receiver to run the same model on the same text; it cannot be
done for a cross-family mapper such as SwaT, which has nothing to recompute against.

The signature uses Ed25519 from `cryptography` (not a dependency of the steering engine itself).
"""
from __future__ import annotations

import hashlib
from dataclasses import dataclass, field

import torch

DOMAIN = "playbox.cache-handoff.v1"


def legacy(past) -> tuple:
    """The cache as a tuple of (key, value) tensors per layer, whatever object the model returned."""
    if hasattr(past, "to_legacy_cache"):
        past = past.to_legacy_cache()
    return tuple((k.detach(), v.detach()) for k, v in past)


def cache_bytes(past) -> bytes:
    h = bytearray()
    for k, v in legacy(past):
        for t in (k, v):
            t = t.to(torch.float32).contiguous().cpu()
            h += str(tuple(t.shape)).encode() + t.numpy().tobytes()
    return bytes(h)


def cache_digest(past) -> str:
    return hashlib.sha256(cache_bytes(past)).hexdigest()


def text_digest(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def _body(weights: str, text_sha: str, cache_sha: str, signer: str) -> bytes:
    return f"{DOMAIN}\nweights {weights}\ntext {text_sha}\ncache {cache_sha}\nsigner {signer}\n".encode()


@dataclass(frozen=True)
class Record:
    weights: str
    text_sha: str
    cache_sha: str
    signer: str  # hex public key
    sig: str  # hex


def commit(private_key, weights: str, text: str, past) -> Record:
    """Sign a commitment to this text and this exact cache. `private_key` is a cryptography Ed25519PrivateKey."""
    from cryptography.hazmat.primitives import serialization

    pub = private_key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    ts, cs = text_digest(text), cache_digest(past)
    return Record(weights, ts, cs, pub, private_key.sign(_body(weights, ts, cs, pub)).hex())


def signature_ok(rec: Record) -> bool:
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    try:
        Ed25519PublicKey.from_public_bytes(bytes.fromhex(rec.signer)).verify(bytes.fromhex(rec.sig), _body(rec.weights, rec.text_sha, rec.cache_sha, rec.signer))
        return True
    except (InvalidSignature, ValueError):
        return False


@dataclass
class Verdict:
    ok: bool
    reasons: list[str] = field(default_factory=list)
    max_abs_diff: float | None = None  # largest entry-wise difference from the recomputed cache (when audited)


def max_cache_diff(a, b) -> float:
    la, lb = legacy(a), legacy(b)
    if len(la) != len(lb):
        return float("inf")
    worst = 0.0
    for (ka, va), (kb, vb) in zip(la, lb):
        if ka.shape != kb.shape or va.shape != vb.shape:
            return float("inf")
        worst = max(worst, float((ka - kb).abs().max()), float((va - vb).abs().max()))
    return worst


def verify(rec: Record, text: str, past, expected_weights: str, trusted_signers: set[str], recompute=None, tol: float = 1e-3) -> Verdict:
    """Steps 1 and 2 always; step 3 only when `recompute` (a function from text to a clean cache) is given."""
    why = []
    if rec.signer not in trusted_signers:
        why.append("signer is not trusted")
    if not signature_ok(rec):
        why.append("bad signature")
    if rec.weights != expected_weights:
        why.append("record is for different weights")
    if rec.text_sha != text_digest(text):
        why.append("record is about different text")
    if rec.cache_sha != cache_digest(past):
        why.append("received cache is not the one the record commits to")
    diff = None
    if recompute is not None and not why:
        diff = max_cache_diff(past, recompute(text))
        if not diff <= tol:
            why.append(f"cache does not come from this text (max entry difference {diff:.4g} > {tol:g})")
    return Verdict(not why, why, diff)


def detection_probability(p: float, n: int) -> float:
    """Chance an audit of each handoff with probability p catches a sender who cheats n times."""
    return 1.0 - (1.0 - p) ** n
