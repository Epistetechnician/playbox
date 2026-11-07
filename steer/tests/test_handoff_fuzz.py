"""Mutation tests for the KV-cache handoff a peer sends (`PBH1`). `Receiver.accept` takes bytes from another member and must
always answer with a `Decision`: never raise, never hang, and never accept anything whose cache differs from what the signed
record covers. The mutations are seeded, so a failure reproduces."""
import json
import random
import struct

import pytest
import torch

pytest.importorskip("cryptography")
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from safetensors.torch import load as st_load
from safetensors.torch import save as st_save

from playbox_steer import engine, handoff, latent

W = "ab" * 32
TEXT = "The movie was"


@pytest.fixture(scope="module")
def world(gpt2_dir):
    model, tok = engine.load(gpt2_dir)
    key = Ed25519PrivateKey.generate()
    pub = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    clean = lambda t: latent.read_context(model, tok, t, None)  # noqa: E731
    data = handoff.pack(key, W, TEXT, clean(TEXT))
    return {"clean": clean, "pub": pub, "data": data}


def split(data: bytes):
    (n,) = struct.unpack(">I", data[4:8])
    return json.loads(data[8:8 + n]), st_load(data[8 + n:])


def build(head, tensors) -> bytes:
    hb = json.dumps(head, sort_keys=True).encode()
    return handoff.MAGIC + struct.pack(">I", len(hb)) + hb + st_save(tensors)


def byte_mutation(rng: random.Random, data: bytes) -> bytes:
    b = bytearray(data)
    for _ in range(rng.randint(1, 4)):
        if not b:
            break  # truncated to nothing: an empty message is a valid input too
        at = rng.randrange(len(b))
        c = rng.randrange(4)
        if c == 0:
            b[at] ^= 1 << rng.randrange(8)
        elif c == 1:
            del b[at:]
        elif c == 2:
            del b[at:at + rng.randint(1, 16)]
        else:
            b[at:at] = bytes(rng.randrange(256) for _ in range(rng.randint(1, 8)))
    return bytes(b)


ODD = [None, 1, -1, 1.5, True, [], {}, "", "zz", [1], {"a": 1}, "\u0000", 1e308]


def structured_mutation(rng: random.Random, data: bytes) -> bytes:
    head, tensors = split(data)
    c = rng.randrange(7)
    if c == 0:  # a record field of the wrong type
        k = rng.choice(list(head["record"]))
        head["record"][k] = rng.choice(ODD)
    elif c == 1:  # the text of the wrong type, or missing
        head["text"] = rng.choice(ODD)
    elif c == 2:  # a tensor of the wrong shape or type
        k = rng.choice(list(tensors))
        tensors[k] = rng.choice([torch.zeros(()), torch.zeros(3), torch.zeros(2, 2, 2, 2, 2), torch.zeros(1, 2, 3, dtype=torch.int64),
                                 torch.full_like(tensors[k], float("nan")), torch.full_like(tensors[k], float("inf")), torch.zeros(0)])
    elif c == 3:  # a missing tensor
        del tensors[rng.choice(list(tensors))]
    elif c == 4:  # an unexpected tensor
        tensors["extra"] = torch.zeros(2)
    elif c == 5:  # the cache of another length
        k = rng.choice([k for k in tensors if k.startswith("k")])
        t = tensors[k]
        tensors[k] = t[..., : max(1, t.shape[-2] // 2), :].contiguous() if t.dim() >= 2 else t
    else:  # a whole record replaced by something that is not one
        head["record"] = rng.choice(ODD)
    return build(head, {k: v.contiguous() for k, v in tensors.items()})


def test_a_valid_handoff_is_still_accepted(world):
    d, past = handoff.Receiver(W, {world["pub"]}, world["clean"], audit_prob=1.0, tol=0.0).accept(world["data"])
    assert d.accepted and past is not None


@pytest.mark.parametrize("audit_prob", [0.0, 1.0])
def test_mutated_handoffs_get_a_decision_never_an_exception(world, audit_prob):
    rng = random.Random(7)
    r = handoff.Receiver(W, {world["pub"]}, world["clean"], audit_prob=audit_prob, tol=0.0)
    accepted = 0
    for i in range(250):
        data = byte_mutation(rng, world["data"]) if i % 2 == 0 else structured_mutation(rng, world["data"])
        d, past = r.accept(data)
        assert d.accepted == (past is not None)
        accepted += d.accepted
    # Almost nothing survives mutation; what does must have been a change the signed record does not cover (none exist today).
    assert accepted <= 3, f"{accepted} of 250 mutated handoffs were accepted"
