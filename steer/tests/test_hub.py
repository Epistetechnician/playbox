"""Hub-and-spoke latent channel on synthetic "models": states are a shared concept space seen through a different linear
lens per model, plus model-private directions and noise. Ground truth is known, so transfer can be scored exactly."""
import pytest
import torch

from playbox_steer import hub as H

Q = 20  # shared concept dimensions
DIMS = {"a": 96, "b": 160, "c": 224, "d": 64}


def make(names, n=300, seed=0, noise=0.05):
    g = torch.Generator().manual_seed(seed)
    lens = {m: torch.randn(Q, DIMS[m], generator=g) for m in DIMS}
    priv = {m: torch.randn(8, DIMS[m], generator=g) * 0.5 for m in DIMS}

    def states(c, m, with_noise=True):
        p = torch.randn(c.shape[0], 8, generator=g)
        x = c @ lens[m] + p @ priv[m]
        return x + noise * torch.randn(x.shape, generator=g) if with_noise else x

    c = torch.randn(n, Q, generator=g)
    return lens, {m: states(c, m) for m in names}


def cos(a, b):
    return torch.nn.functional.cosine_similarity(a, b, dim=-1)


def test_transfer_between_every_ordered_pair_beats_random():
    names = ["a", "b", "c", "d"]
    lens, st = make(names)
    hub = H.fit_hub(st, "anchors", k=Q)
    g = torch.Generator().manual_seed(7)
    new = torch.randn(40, Q, generator=g)  # concepts the hub never saw
    for s in names:
        for d in names:
            if s == d:
                continue
            got = hub.send(s, d, new @ lens[s])
            assert cos(got, new @ lens[d]).mean() > 0.9, (s, d)
            rand = torch.randn(40, DIMS[s], generator=g)
            assert cos(hub.send(s, d, rand), new @ lens[d]).abs().mean() < 0.35, (s, d)


def test_a_new_model_joins_without_refitting_the_others():
    lens, st = make(["a", "b", "c", "d"])
    hub = H.fit_hub({m: st[m] for m in "abc"}, "anchors", k=Q)
    before = {m: (hub.spokes[m].enc.clone(), hub.spokes[m].dec.clone()) for m in "abc"}
    hub.add_spoke("d", st["d"])
    for m in "abc":
        assert torch.equal(hub.spokes[m].enc, before[m][0]) and torch.equal(hub.spokes[m].dec, before[m][1])
    new = torch.randn(30, Q, generator=torch.Generator().manual_seed(3))
    assert cos(hub.send("d", "a", new @ lens["d"]), new @ lens["a"]).mean() > 0.85
    assert cos(hub.send("b", "d", new @ lens["b"]), new @ lens["d"]).mean() > 0.85


def test_int8_over_the_wire_costs_little_and_is_k_bytes():
    lens, st = make(["a", "b"])
    hub = H.fit_hub(st, "anchors", k=Q)
    new = torch.randn(20, Q, generator=torch.Generator().manual_seed(5))
    for v in new @ lens["a"]:
        h = hub.encode("a", v)
        blob = H.pack(hub, "a", h)
        head, h2 = H.unpack(hub, blob)
        assert head["k"] == Q
        assert cos(hub.decode("b", h), hub.decode("b", h2)) > 0.99
    raw, _ = H.quantize(h)
    assert len(raw) == Q


def test_a_message_is_refused_unless_it_is_well_formed_and_for_this_hub():
    lens, st = make(["a", "b"])
    hub = H.fit_hub(st, "anchors", k=Q)
    other = H.fit_hub(st, "other anchors", k=Q - 2)
    h = hub.encode("a", torch.randn(DIMS["a"]))
    blob = H.pack(hub, "a", h)
    H.unpack(hub, blob)
    for bad, why in [
        (b"XXXX" + blob[4:], "magic"),
        (blob[:-1], "truncated body"),
        (blob[:6], "truncated"),
        (blob[:-1] + bytes([blob[-1] ^ 1]), "tampered body"),
        (blob + b"\0", "extra byte"),
        (blob * 10000, "oversized"),
    ]:
        with pytest.raises(ValueError):
            H.unpack(hub, bad)
    with pytest.raises(ValueError):
        H.unpack(other, blob)  # a different hub


def test_spokes_must_use_the_same_anchors():
    _, st = make(["a", "b"])
    hub = H.fit_hub(st, "anchors", k=Q)
    with pytest.raises(ValueError):
        hub.add_spoke("c", torch.randn(10, 224))
    with pytest.raises(ValueError):
        H.fit_hub({"a": st["a"], "b": st["b"][:100]}, "anchors")


def test_private_directions_do_not_cross():
    """A direction only one model has (outside the shared concept space) is not carried to the others."""
    lens, st = make(["a", "b"], noise=0.01)
    hub = H.fit_hub(st, "anchors", k=Q)
    g = torch.Generator().manual_seed(11)
    q, _ = torch.linalg.qr(lens["a"].T)  # basis of a's concept subspace
    v = torch.randn(DIMS["a"], generator=g)
    v = v - q @ (q.T @ v)  # orthogonal to every shared concept direction
    shared = (torch.randn(1, Q, generator=g) @ lens["a"])[0]
    assert hub.encode("a", v).norm() < 0.35 * hub.encode("a", shared).norm() * v.norm() / shared.norm()


def test_signed_envelope_round_trips_and_refuses_tampering_and_strangers():
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives import serialization

    lens, st = make(["a", "b"])
    hub = H.fit_hub(st, "anchors", k=Q)
    key, other = Ed25519PrivateKey.generate(), Ed25519PrivateKey.generate()
    pub = lambda k: k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    h = hub.encode("a", torch.randn(1, Q).squeeze(0) @ lens["a"])
    blob = H.pack_signed(hub, "a", h, key)
    head, got = H.unpack_signed(hub, blob, trusted_signers={pub(key)})
    assert head["signer"] == pub(key) and len(blob) < 600
    assert cos(got, h).item() > 0.999  # int8 with a float32 scale costs almost nothing
    for bad, why in [
        (blob[:-1], "body length"),
        (blob[:-1] + bytes([blob[-1] ^ 1]), "body does not match"),
        (blob.replace(b'"src":"a"', b'"src":"b"'), "signature"),
        (b"PBL1" + blob[4:], "not a PBL2"),
    ]:
        with pytest.raises(ValueError, match=why):
            H.unpack_signed(hub, bad)
    with pytest.raises(ValueError, match="not trusted"):
        H.unpack_signed(hub, H.pack_signed(hub, "a", h, other), trusted_signers={pub(key)})
    other_hub = H.fit_hub(make(["a", "b"], seed=3)[1], "anchors2", k=Q)
    with pytest.raises(ValueError, match="different hub"):
        H.unpack_signed(other_hub, blob)


def test_guard_blocks_only_denied_classes_near_a_listed_direction():
    from playbox_steer.latent_room import guard_blocks

    g = torch.zeros(8)
    g[0] = 1.0
    guards = {"unsafe": (g, "harm"), "benign": (g, "style")}
    near = torch.tensor([1.0, 0.2, 0, 0, 0, 0, 0, 0])
    far = torch.tensor([0.0, 1.0, 0, 0, 0, 0, 0, 0])
    assert guard_blocks(near, guards, {"harm"}) == ["unsafe"]
    assert guard_blocks(-near, guards, {"harm"}) == ["unsafe"]  # sign does not matter
    assert guard_blocks(far, guards, {"harm"}) == []
    assert guard_blocks(near, guards, set()) == []


def test_commit_and_challenge_audit_passes_honest_senders_and_catches_swaps_and_silence():
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    from playbox_steer import latent_audit as LA

    lens, st = make(["a", "b"])
    hub = H.fit_hub(st, "anchors", k=Q)
    key = Ed25519PrivateKey.generate()
    g = torch.Generator().manual_seed(11)
    concepts = torch.randn(6, Q, generator=g)
    sentences = {i: [f"sentence {i}.{j}" for j in range(3)] for i in range(6)}
    by_text = {tuple(sentences[i]): concepts[i] for i in range(6)}
    thought_b = lambda sents: by_text[tuple(sents)] @ lens["b"]  # what auditor b would compute from the revealed sentences
    salt = "00" * 16

    def send(true_i, sent_i, reveal=True, salt_used=salt):
        h = hub.encode("a", concepts[true_i] @ lens["a"])  # the vector actually sent
        commit = H.make_commit(salt, sentences[sent_i])    # what the sender committed to having read
        head, vec = H.unpack_signed(hub, H.pack_signed(hub, "a", h, key, commit=commit))
        rev = H.reveal_bytes(salt_used, sentences[sent_i]) if reveal else None
        return LA.audit(hub, "b", head, vec, rev, thought_b, threshold=0.5)

    honest = [send(i, i) for i in range(6)]
    assert all(v.ok for v in honest), [v.cosine for v in honest]
    swapped = [send(i, (i + 1) % 6) for i in range(6)]  # commits to sentence set i+1, sends the vector of concept i
    assert not any(v.ok for v in swapped) and max(v.cosine for v in swapped) < min(v.cosine for v in honest)
    assert "did not reveal" in send(0, 0, reveal=False).reasons[0]
    assert "does not match the commitment" in send(0, 0, salt_used="11" * 16).reasons[0]
    # No commitment at all cannot be audited.
    h = hub.encode("a", concepts[0] @ lens["a"])
    head, vec = H.unpack_signed(hub, H.pack_signed(hub, "a", h, key))
    assert LA.audit(hub, "b", head, vec, b"{}", thought_b, 0.5).reasons == ["the message carries no commitment"]
    # The commitment is signed: stripping or replacing it breaks the signature.
    blob = H.pack_signed(hub, "a", h, key, commit=H.make_commit(salt, sentences[0]))
    with pytest.raises(ValueError, match="signature"):
        H.unpack_signed(hub, blob.replace(H.make_commit(salt, sentences[0]).encode(), H.make_commit(salt, sentences[1]).encode()))
