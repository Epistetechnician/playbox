"""The audit on the tiny random GPT-2: honest caches pass, any tampering fails, and the record is bound to both."""
import dataclasses

import pytest
import torch

pytest.importorskip("cryptography")
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

from playbox_steer import cache_audit as ca
from playbox_steer import engine, latent

W = "ab" * 32
TEXT = "The movie was"


def setup(gpt2_dir):
    model, tok = engine.load(gpt2_dir)
    key = Ed25519PrivateKey.generate()
    pub = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    return model, tok, key, pub


def steered_cache(model, tok, text, coef, seed=1):
    d = torch.tensor(engine.random_direction(model.config.hidden_size, seed))
    spec = latent._spec(1, coef, "prompt")
    norm = engine.calibrate(model, tok, spec, type("P", (), {"prompts": (text,)})())
    with engine.Steerer(model, spec, d, norm):
        return latent.read_context(model, tok, text, None)


def test_an_honest_handoff_passes_with_zero_difference(gpt2_dir):
    model, tok, key, pub = setup(gpt2_dir)
    past = latent.read_context(model, tok, TEXT, None)
    rec = ca.commit(key, W, TEXT, past)
    v = ca.verify(rec, TEXT, past, W, {pub}, recompute=lambda t: latent.read_context(model, tok, t, None), tol=0.0)
    assert v.ok, v.reasons
    assert v.max_abs_diff == 0.0, "the same model on the same text recomputes bit for bit"


def test_a_steered_cache_with_a_valid_signature_is_caught_only_by_the_recompute(gpt2_dir):
    """A lying sender signs the digest of the steered cache it sends. Steps 1 and 2 pass; the audit fails."""
    model, tok, key, pub = setup(gpt2_dir)
    bad = steered_cache(model, tok, TEXT, 4.0)
    rec = ca.commit(key, W, TEXT, bad)
    cheap = ca.verify(rec, TEXT, bad, W, {pub}, recompute=None)
    assert cheap.ok, "the record is internally consistent, so the cheap checks cannot tell"
    audited = ca.verify(rec, TEXT, bad, W, {pub}, recompute=lambda t: latent.read_context(model, tok, t, None), tol=1e-3)
    assert not audited.ok and "does not come from this text" in audited.reasons[0]


def test_even_a_tiny_steer_is_caught_at_exact_tolerance(gpt2_dir):
    model, tok, key, pub = setup(gpt2_dir)
    tiny = steered_cache(model, tok, TEXT, 0.001)
    rec = ca.commit(key, W, TEXT, tiny)
    v = ca.verify(rec, TEXT, tiny, W, {pub}, recompute=lambda t: latent.read_context(model, tok, t, None), tol=0.0)
    assert not v.ok and v.max_abs_diff > 0


def test_the_record_is_bound_to_the_cache_text_weights_and_signer(gpt2_dir):
    model, tok, key, pub = setup(gpt2_dir)
    past = latent.read_context(model, tok, TEXT, None)
    other = latent.read_context(model, tok, "Today the food seems", None)
    rec = ca.commit(key, W, TEXT, past)
    assert "received cache is not the one" in ca.verify(rec, TEXT, other, W, {pub}).reasons[0]
    assert "different text" in ca.verify(rec, "something else", past, W, {pub}).reasons[0]
    assert "different weights" in ca.verify(rec, TEXT, past, "cd" * 32, {pub}).reasons[0]
    assert "not trusted" in ca.verify(rec, TEXT, past, W, {"00" * 32}).reasons[0]
    forged = dataclasses.replace(rec, cache_sha=ca.cache_digest(other))
    assert "bad signature" in ca.verify(forged, TEXT, other, W, {pub}).reasons


def test_sampled_audits_catch_a_repeat_cheater_with_the_stated_probability():
    assert ca.detection_probability(0.1, 1) == pytest.approx(0.1)
    assert ca.detection_probability(0.1, 22) > 0.9
    assert ca.detection_probability(0.0, 100) == 0.0
