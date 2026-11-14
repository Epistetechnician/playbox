"""A latent handoff as one message: the sender packs a signed record, the text it claims and the cache; the receiver
decides whether to use it.

Wire format (everything the receiver needs, no pickle): `PBH1`, a 4-byte big-endian length, a JSON header
({record, text}), then the cache as a safetensors blob. `torch.load` is never used on peer bytes.

The receiver's policy (`Receiver`):
  1. Always: the record's signature, signer, weights and text hash, and that the cache bytes match the record.
  2. With probability `audit_prob`, drawn from the OS random source so the sender cannot predict it: recompute the
     cache from the text and compare entry by entry (the audit from `cache_audit`).
  3. A signer that fails any check is quarantined and refused from then on.
The audit is the only check that catches a sender who signs a cache it did not compute from the text. A judge of
B's output cannot replace it: judging needs a clean reference, and producing one is the same recompute.
"""
from __future__ import annotations

import dataclasses
import json
import secrets
import struct
from dataclasses import asdict, dataclass, field

import torch
from safetensors.torch import load as st_load
from safetensors.torch import save as st_save
from transformers import DynamicCache

from . import cache_audit as ca

MAGIC = b"PBH1"
MAX_BYTES = 512 * 1024 * 1024
MAX_HEADER = 4 * 1024 * 1024


class HandoffError(ValueError):
    pass


def pack(private_key, weights: str, text: str, past) -> bytes:
    """Sign and serialise a handoff. `past` is the cache the sender actually sends."""
    rec = ca.commit(private_key, weights, text, past)
    header = json.dumps({"record": asdict(rec), "text": text}, sort_keys=True).encode()
    tensors = {}
    for i, (k, v) in enumerate(ca.legacy(past)):
        tensors[f"k{i}"], tensors[f"v{i}"] = k.contiguous().cpu(), v.contiguous().cpu()
    return MAGIC + struct.pack(">I", len(header)) + header + st_save(tensors)


def unpack(data: bytes) -> tuple[ca.Record, str, DynamicCache]:
    if len(data) > MAX_BYTES or len(data) < 8 or data[:4] != MAGIC:
        raise HandoffError("not a handoff message")
    (n,) = struct.unpack(">I", data[4:8])
    if n > MAX_HEADER or 8 + n > len(data):
        raise HandoffError("bad header length")
    try:
        head = json.loads(data[8 : 8 + n])
        rec = ca.Record(**head["record"])
        text = head["text"]
        tensors = st_load(data[8 + n :])
    except Exception as e:  # malformed JSON, missing fields, bad safetensors
        raise HandoffError(f"malformed handoff: {type(e).__name__}") from None
    if not isinstance(text, str):
        raise HandoffError("text must be a string")
    # The record is a plain dataclass, so JSON of any type got through `Record(**...)`; a signer that is a list would crash
    # the receiver at its first set lookup. Every field is text, or the message is refused here.
    if not all(isinstance(getattr(rec, f.name), str) for f in dataclasses.fields(rec)):
        raise HandoffError("every field of the record must be text")
    layers = []
    i = 0
    while f"k{i}" in tensors:
        if f"v{i}" not in tensors:
            raise HandoffError("cache has a key without a value")
        layers.append((tensors[f"k{i}"], tensors[f"v{i}"]))
        i += 1
    if not layers or len(tensors) != 2 * len(layers):
        raise HandoffError("cache layers are missing or unexpected tensors are present")
    if any(t.is_floating_point() and not bool(torch.isfinite(t).all()) for kv in layers for t in kv):
        raise HandoffError("cache holds NaN or infinite values")      # a signer can sign these; they poison every later token
    try:
        return rec, text, DynamicCache.from_legacy_cache(tuple(layers))
    except Exception as e:  # tensors of a shape or type a cache cannot hold
        raise HandoffError(f"malformed cache: {type(e).__name__}") from None


@dataclass
class Decision:
    accepted: bool
    audited: bool = False
    reasons: list[str] = field(default_factory=list)
    max_abs_diff: float | None = None


class Receiver:
    def __init__(self, expected_weights: str, trusted_signers: set[str], recompute, audit_prob: float = 0.1, tol: float = 1e-3,
                 draw=None):
        if not 0.0 <= audit_prob <= 1.0:
            raise ValueError("audit_prob must be in [0, 1]")
        self.weights, self.trusted, self.recompute, self.p, self.tol = expected_weights, set(trusted_signers), recompute, audit_prob, tol
        self.draw = draw or (lambda: secrets.randbelow(10**9) / 10**9)
        self.quarantined: set[str] = set()
        self.stats = {"received": 0, "accepted": 0, "audited": 0, "caught": 0}

    def accept(self, data: bytes) -> tuple[Decision, DynamicCache | None]:
        """Returns the decision and, only if accepted, the cache to continue from."""
        self.stats["received"] += 1
        try:
            rec, text, past = unpack(data)
        except HandoffError as e:
            return Decision(False, reasons=[str(e)]), None
        if rec.signer in self.quarantined:
            return Decision(False, reasons=["signer is quarantined"]), None
        audit = self.draw() < self.p
        v = ca.verify(rec, text, past, self.weights, self.trusted, recompute=self.recompute if audit else None, tol=self.tol)
        self.stats["audited"] += audit
        if not v.ok:
            if rec.signer in self.trusted and audit and v.max_abs_diff is not None:
                self.stats["caught"] += 1
            if rec.signer in self.trusted and ca.signature_ok(rec):
                # Only a valid signature proves the key itself produced the bad cache. Without that check a stranger
                # could forge a record naming a trusted signer and get it quarantined.
                self.quarantined.add(rec.signer)
            return Decision(False, audit, v.reasons, v.max_abs_diff), None
        self.stats["accepted"] += 1
        return Decision(True, audit, [], v.max_abs_diff), past
