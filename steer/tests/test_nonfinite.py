"""Numeric edge cases on peer-supplied inputs that need no model: NaN and inf must never read as "close enough"."""
import json
import struct

import pytest
import torch

from playbox_steer import cache_audit as ca
from playbox_steer import hub as H
from playbox_steer import latent_audit as la


def cache(fill=None):
    k = torch.ones(1, 2, 3, 4)
    v = torch.ones(1, 2, 3, 4)
    if fill is not None:
        k = k * fill
    return ((k, v),)


def test_an_all_nan_cache_is_not_zero_distance_from_a_clean_one():
    assert ca.max_cache_diff(cache(float("nan")), cache()) == float("inf")
    assert ca.max_cache_diff(cache(), cache(float("nan"))) == float("inf")
    assert ca.max_cache_diff(cache(float("inf")), cache()) == float("inf")
    assert ca.max_cache_diff(cache(), cache()) == 0.0


def test_verify_fails_the_audit_of_a_nan_cache():
    pytest.importorskip("cryptography")
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    key = Ed25519PrivateKey.generate()
    pub = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    bad = cache(float("nan"))
    rec = ca.commit(key, "ab" * 32, "text", bad)
    v = ca.verify(rec, "text", bad, "ab" * 32, {pub}, recompute=lambda t: cache(), tol=1e-3)
    assert not v.ok and any("does not come from this text" in r for r in v.reasons)


def test_a_signed_nan_cache_is_refused_at_unpack():
    pytest.importorskip("transformers")
    from playbox_steer import handoff

    head = json.dumps({"record": {"weights": "w", "text_sha": "t", "cache_sha": "c", "signer": "s", "sig": "g"}, "text": "x"}).encode()
    body = handoff.st_save({"k0": torch.full((1, 1, 2, 2), float("nan")), "v0": torch.ones(1, 1, 2, 2)})
    with pytest.raises(handoff.HandoffError):
        handoff.unpack(handoff.MAGIC + struct.pack(">I", len(head)) + head + body)


def test_latent_audit_does_not_pass_a_nan_cosine(monkeypatch):
    monkeypatch.setattr(H, "make_commit", lambda salt, s: "c" * 64)
    hub = type("Hub", (), {"encode": lambda self, name, thought: thought})()
    head = {"commit": "c" * 64}
    reveal = b'{"salt":"' + b"ab" * 8 + b'","sentences":["one","two"]}'
    vec = torch.tensor([1.0, 0.0])
    assert la.audit(hub, "me", head, vec, reveal, lambda s: torch.tensor([1.0, 0.0]), 0.5).ok
    for bad in (torch.tensor([float("nan"), 1.0]), torch.tensor([float("inf"), 1.0])):
        v = la.audit(hub, "me", head, vec, reveal, lambda s, b=bad: b, 0.5)
        assert not v.ok, "a non-finite thought is not evidence the vector follows from the sentences"


def _hub():
    g = torch.Generator().manual_seed(0)
    return H.fit_hub({"a": torch.randn(40, 12, generator=g), "b": torch.randn(40, 9, generator=g)}, "x", k=4), g


def test_pbl1_scale_too_big_for_a_float_is_a_value_error():
    hub, g = _hub()
    blob = H.pack(hub, "a", torch.randn(4, generator=g))
    n = struct.unpack(">I", blob[4:8])[0]
    head = json.loads(blob[8:8 + n])
    head["scale"] = 10 ** 400
    hb = json.dumps(head).encode()
    with pytest.raises(ValueError):
        H.unpack(hub, H.MAGIC + struct.pack(">I", len(hb)) + hb + blob[8 + n:])


def test_quantize_refuses_a_non_finite_thought():
    for bad in (float("nan"), float("inf")):
        with pytest.raises(ValueError):
            H.quantize(torch.tensor([1.0, bad]))
