"""Mutation tests for the parsers of hub messages. A message comes from another member (or from a host that relays it), so
whatever the bytes, `unpack`, `unpack_signed` and the audit must either return a result or raise `ValueError`: a caller that
handles `ValueError` handles every malformed message. The mutations are seeded, so a failure reproduces."""
import json
import random
import struct

import blake3
import pytest
import torch
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from playbox_steer import hub as H
from playbox_steer import latent_audit as LA

SENTENCES = ["Waves rolled across the open sea."]
SALT = "00112233445566778899aabbccddeeff"


@pytest.fixture(scope="module")
def world():
    g = torch.Generator().manual_seed(0)
    st = {m: torch.randn(200, d, generator=g) for m, d in {"a": 40, "b": 60}.items()}
    hub = H.fit_hub(st, "anchors", k=20)
    key = Ed25519PrivateKey.generate()
    signer = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    commit = H.make_commit(SALT, SENTENCES)
    h = hub.encode("a", torch.randn(40, generator=g))
    return {"hub": hub, "key": key, "signer": signer, "commit": commit, "h": h,
            "v2": H.pack_signed(hub, "a", h, key, commit=commit), "v1": H.pack(hub, "a", h),
            "reveal": H.reveal_bytes(SALT, SENTENCES)}


EDGES = [b"null", b"-1", b"0", b"1e999", b'""', b"[]", b"{}", b"true", b'"\xff"', b"NaN", b"[1]", b'{"a":1}']


def mutate_bytes(rng: random.Random, blob: bytes) -> bytes:
    b = bytearray(blob)
    for _ in range(rng.randint(1, 4)):
        if not b:
            b.append(ord("{"))
        at = rng.randrange(len(b))
        c = rng.randrange(6)
        if c == 0:
            b[at] ^= 1 << rng.randrange(8)
        elif c == 1:
            del b[at:]
        elif c == 2:
            del b[at:at + rng.randint(1, 16)]
        elif c == 3:
            b[at:at] = bytes(rng.randrange(256) for _ in range(rng.randint(1, 8)))
        elif c == 4:
            b[at:at + rng.randint(0, 8)] = rng.choice(EDGES)
        else:
            b[at:at] = b[at:at + rng.randint(1, 32)]
    return bytes(b)


ODD = [None, 1, -1, 1.5, True, [], {}, "", "zz", "00", "0" * 7, "0" * 9, [1], {"a": 1}, "\u0000", 1e308, float("nan")]


def mutate_header(rng: random.Random, blob: bytes, magic: bytes) -> bytes:
    """Rewrite one header field to a value of another type, keeping the length prefix consistent."""
    (n,) = struct.unpack(">I", blob[4:8])
    head = json.loads(blob[8:8 + n])
    raw = blob[8 + n:]
    if rng.random() < 0.15:
        head = rng.choice([[], [1], "x", 1, None])
    else:
        k = rng.choice(list(head))
        if rng.random() < 0.2:
            del head[k]
        else:
            head[k] = rng.choice(ODD)
    hb = json.dumps(head, separators=(",", ":")).encode()
    return magic + struct.pack(">I", len(hb)) + hb + raw


def only_value_errors(fn) -> None:
    try:
        fn()
    except ValueError:
        pass


def test_a_valid_message_still_round_trips(world):
    head, vec = H.unpack_signed(world["hub"], world["v2"], trusted_signers={world["signer"]})
    assert head["signer"] == world["signer"] and head["commit"] == world["commit"]
    assert torch.allclose(vec, world["h"], atol=world["h"].abs().max().item() / 100)
    H.unpack(world["hub"], world["v1"])


def test_mutated_messages_only_ever_raise_value_error(world):
    rng = random.Random(1)
    hub, trusted = world["hub"], {world["signer"]}
    for i in range(4000):
        blob = [mutate_bytes(rng, world["v2"]), mutate_header(rng, world["v2"], H.MAGIC2),
                mutate_bytes(rng, world["v1"]), mutate_header(rng, world["v1"], H.MAGIC)][i % 4]
        only_value_errors(lambda: H.unpack_signed(hub, blob, trusted_signers=trusted if i % 3 else None))
        only_value_errors(lambda: H.unpack(hub, blob))


def signed_with(world, **fields) -> bytes:
    """A message whose header the member signed itself, so the signature is valid and the parser reads on."""
    hub = world["hub"]
    f = {"hub": hub.hub_id, "src": "a", "k": hub.k, "scale_f32": "3c4985f0",
         "body_blake3": blake3.blake3(bytes(hub.k)).hexdigest(), "signer": world["signer"], **fields}
    sig = world["key"].sign(H._signed_text(f["hub"], f["src"], f["k"], f["scale_f32"], f["body_blake3"], f["signer"], None)).hex()
    hb = json.dumps({**f, "sig": sig}, separators=(",", ":")).encode()
    return H.MAGIC2 + struct.pack(">I", len(hb)) + hb + bytes(hub.k)


@pytest.mark.parametrize("scale", ["00", "", "zzzzzzzz", "3c4985f0aa", "7fc00000", "ff800000", "00000000", "7f800000"])
def test_a_member_cannot_crash_a_receiver_with_a_signed_malformed_scale(world, scale):
    # Signed by a trusted member, so only the parser stands between the bytes and the receiver.
    with pytest.raises(ValueError):
        H.unpack_signed(world["hub"], signed_with(world, scale_f32=scale), trusted_signers={world["signer"]})


def test_specific_malformed_headers_are_value_errors(world):
    hub = world["hub"]
    for head in (None, 1, "x", [], [1], {"hub": []}, {"hub": hub.hub_id, "k": True}, {"hub": hub.hub_id, "k": hub.k}):
        hb = json.dumps(head).encode()
        for magic, fn in ((H.MAGIC2, H.unpack_signed), (H.MAGIC, H.unpack)):
            with pytest.raises(ValueError):
                fn(hub, magic + struct.pack(">I", len(hb)) + hb + bytes(hub.k))
    deep = b"[" * 3000 + b"]" * 3000
    for magic, fn in ((H.MAGIC2, H.unpack_signed), (H.MAGIC, H.unpack)):
        with pytest.raises(ValueError):
            fn(hub, magic + struct.pack(">I", len(deep)) + deep + bytes(hub.k))
    # An unhashable or non-text signer must not reach the trusted-set lookup.
    with pytest.raises(ValueError):
        H.unpack_signed(hub, signed_with(world, signer=["x"]), trusted_signers={world["signer"]})
    # A bad scale in a PBL1 header.
    for scale in (None, True, [1], "0.5", -1, 0, 1e9):
        head = {"hub": hub.hub_id, "src": "a", "k": hub.k, "scale": scale, "body_sha256": __import__("hashlib").sha256(bytes(hub.k)).hexdigest()}
        hb = json.dumps(head).encode()
        with pytest.raises(ValueError):
            H.unpack(hub, H.MAGIC + struct.pack(">I", len(hb)) + hb + bytes(hub.k))


def audit(world, reveal: bytes):
    thought = lambda s: torch.randn(60)  # noqa: E731
    return LA.audit(world["hub"], "b", {"commit": world["commit"]}, world["h"], reveal, thought, 0.4)


def test_the_audit_treats_every_malformed_reveal_as_a_failed_audit_not_an_exception(world):
    rng = random.Random(2)
    for _ in range(3000):
        v = audit(world, mutate_bytes(rng, world["reveal"]))
        assert v.ok in (True, False)
    too_many = H.reveal_bytes(SALT, ["s"] * (LA.MAX_REVEAL_SENTENCES + 1))
    too_long = H.reveal_bytes(SALT, ["x" * (LA.MAX_SENTENCE_BYTES + 1)])
    newline = H.reveal_bytes(SALT, ["two\nlines"])
    huge = json.dumps({"salt": SALT, "sentences": ["a" * 100] * 600}).encode()
    for bad in (too_many, too_long, newline, huge, b'{"salt":5,"sentences":["a"]}', b'{"salt":"%s","sentences":[1]}' % SALT.encode(),
                b'{"salt":"%s","sentences":"a"}' % SALT.encode(), b'{"salt":"%s","sentences":["\\ud800"]}' % SALT.encode(),
                b"[" * 5000, b"[]", b"null", b"\xff\xfe"):
        v = audit(world, bad)
        assert not v.ok and v.reasons, bad[:40]


def test_a_prompt_file_nested_thousands_deep_is_refused_as_a_value_error(tmp_path):
    from playbox_steer import files

    p = tmp_path / "deep.json"
    p.write_bytes(b"[" * 100_000 + b"]" * 100_000)
    with pytest.raises(ValueError):
        files.read_prompts(p)
