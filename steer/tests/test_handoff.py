import pytest
import torch

pytest.importorskip("cryptography")
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from playbox_steer import cache_audit as ca
from playbox_steer import engine, handoff, latent

W = "ab" * 32
TEXT = "The movie was"


def parts(gpt2_dir):
    model, tok = engine.load(gpt2_dir)
    key = Ed25519PrivateKey.generate()
    pub = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    clean = lambda t: latent.read_context(model, tok, t, None)
    return model, tok, key, pub, clean


def steered(model, tok, text, coef):
    d = torch.tensor(engine.random_direction(model.config.hidden_size, 3))
    spec = latent._spec(1, coef, "prompt")
    norm = engine.calibrate(model, tok, spec, type("P", (), {"prompts": (text,)})())
    with engine.Steerer(model, spec, d, norm):
        return latent.read_context(model, tok, text, None)


def test_a_handoff_round_trips_bit_for_bit(gpt2_dir):
    model, tok, key, pub, clean = parts(gpt2_dir)
    past = clean(TEXT)
    rec, text, got = handoff.unpack(handoff.pack(key, W, TEXT, past))
    assert text == TEXT and ca.cache_digest(got) == ca.cache_digest(past) == rec.cache_sha


def test_honest_handoffs_are_accepted_whether_or_not_audited(gpt2_dir):
    model, tok, key, pub, clean = parts(gpt2_dir)
    data = handoff.pack(key, W, TEXT, clean(TEXT))
    for p in (0.0, 1.0):
        d, past = handoff.Receiver(W, {pub}, clean, audit_prob=p, tol=0.0).accept(data)
        assert d.accepted and past is not None and d.audited == (p == 1.0)


def test_a_cheating_sender_is_caught_when_audited_and_then_quarantined(gpt2_dir):
    model, tok, key, pub, clean = parts(gpt2_dir)
    bad = handoff.pack(key, W, TEXT, steered(model, tok, TEXT, 2.0))
    r = handoff.Receiver(W, {pub}, clean, audit_prob=1.0)
    d, past = r.accept(bad)
    assert not d.accepted and past is None and "does not come from this text" in d.reasons[0]
    assert r.stats["caught"] == 1 and pub in r.quarantined
    d2, _ = r.accept(handoff.pack(key, W, TEXT, clean(TEXT)))
    assert not d2.accepted and d2.reasons == ["signer is quarantined"]


def test_without_an_audit_a_cheater_passes_which_is_why_the_audit_exists(gpt2_dir):
    model, tok, key, pub, clean = parts(gpt2_dir)
    d, past = handoff.Receiver(W, {pub}, clean, audit_prob=0.0).accept(handoff.pack(key, W, TEXT, steered(model, tok, TEXT, 2.0)))
    assert d.accepted, "documented limit: only the recompute catches a signed steered cache"


def test_tampering_after_signing_fails_the_cheap_checks(gpt2_dir):
    model, tok, key, pub, clean = parts(gpt2_dir)
    other = clean("Today the food seems")
    rec, text, _ = handoff.unpack(handoff.pack(key, W, TEXT, clean(TEXT)))
    swapped = handoff.pack(key, W, "Today the food seems", other)  # a cache and text that match each other, signed
    # Splice: keep this record and text, replace the cache bytes with another cache's.
    a, b = handoff.pack(key, W, TEXT, clean(TEXT)), swapped
    na = 8 + int.from_bytes(a[4:8], "big")
    nb = 8 + int.from_bytes(b[4:8], "big")
    spliced = a[:na] + b[nb:]
    d, _ = handoff.Receiver(W, {pub}, clean, audit_prob=0.0).accept(spliced)
    assert not d.accepted and "not the one the record commits to" in d.reasons[0]


def test_unknown_signer_wrong_weights_and_garbage_are_refused(gpt2_dir):
    model, tok, key, pub, clean = parts(gpt2_dir)
    data = handoff.pack(key, W, TEXT, clean(TEXT))
    assert "not trusted" in handoff.Receiver(W, {"00" * 32}, clean).accept(data)[0].reasons[0]
    assert "different weights" in handoff.Receiver("cd" * 32, {pub}, clean).accept(data)[0].reasons[0]
    r = handoff.Receiver(W, {pub}, clean)
    for junk in (b"", b"PBH1", b"PBH1\xff\xff\xff\xff" + b"x" * 8, b"nope" * 10, data[:50], data[:-5]):
        d, past = r.accept(junk)
        assert not d.accepted and past is None


def test_a_signer_who_is_not_trusted_is_never_added_to_the_quarantine_list(gpt2_dir):
    model, tok, key, pub, clean = parts(gpt2_dir)
    r = handoff.Receiver(W, {"00" * 32}, clean)
    r.accept(handoff.pack(key, W, TEXT, clean(TEXT)))
    assert r.quarantined == set(), "otherwise anyone could grow the list by sending junk with fresh keys"


def test_observed_catch_rate_matches_the_stated_probability(gpt2_dir):
    model, tok, key, pub, clean = parts(gpt2_dir)
    import random
    rng = random.Random(0)
    bad = handoff.pack(key, W, TEXT, steered(model, tok, TEXT, 2.0))
    p, n, trials, caught = 0.3, 4, 150, 0
    for _ in range(trials):
        r = handoff.Receiver(W, {pub}, clean, audit_prob=p, draw=rng.random)
        for _ in range(n):
            d, _ = r.accept(bad)
            if not d.accepted:
                break
        caught += pub in r.quarantined
    assert caught / trials == pytest.approx(ca.detection_probability(p, n), abs=0.1)


def test_a_forged_record_cannot_get_a_trusted_signer_quarantined(gpt2_dir):
    import dataclasses
    model, tok, key, pub, clean = parts(gpt2_dir)
    rec, text, past = handoff.unpack(handoff.pack(key, W, TEXT, clean(TEXT)))
    forged = dataclasses.replace(rec, sig="00" * 64)
    import json, struct
    data = handoff.pack(key, W, TEXT, past)
    n = int.from_bytes(data[4:8], "big")
    head = json.loads(data[8 : 8 + n])
    head["record"]["sig"] = "00" * 64
    nh = json.dumps(head, sort_keys=True).encode()
    forged_msg = b"PBH1" + struct.pack(">I", len(nh)) + nh + data[8 + n :]
    r = handoff.Receiver(W, {pub}, clean, audit_prob=1.0)
    d, _ = r.accept(forged_msg)
    assert not d.accepted and "bad signature" in d.reasons
    assert r.quarantined == set()
    assert r.accept(data)[0].accepted, "the real signer is unaffected"
