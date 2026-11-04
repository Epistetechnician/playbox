"""A shared latent hub for N model families: one small adapter pair per model instead of a map per ordered pair.

`telepathy.py` fits a ridge map for every ordered pair (A -> B), so N models need N*(N-1) maps and a new model needs
2*N more. Here every model gets two linear adapters against one shared k-dimensional space fitted on the same anchor
texts:

    enc_i : R^{d_i} -> R^k     (what model i sends)
    dec_j : R^k     -> R^{d_j} (what model j receives)

A thought crosses i -> j as `dec_j(enc_i(v))`. Only k int8 values cross the wire, whatever d_i and d_j are. A new model
joins by fitting its own two adapters against the existing hub coordinates; nobody else is refitted.

The hub itself is MAXVAR generalized CCA: each model's centred anchor states are whitened by an SVD, the whitened scores
are stacked, and the top-k left singular vectors of the stack are the shared coordinates G (n x k), one row per anchor
text. It captures what the models' states have in common on the anchors and leaves out directions only one model has.

Limits (see docs/latent-channels.md): linear, fitted on a few hundred sentences, so it carries concepts the anchors
span; a model that exposes no activations (API-only) cannot have a spoke and needs an embassy (a local open model that
speaks for it).
"""
from __future__ import annotations

import hashlib
import json
import struct
from dataclasses import dataclass, field

import torch

MAGIC = b"PBL1"


def _ridge_dual(x: torch.Tensor, y: torch.Tensor, lam: float) -> torch.Tensor:
    """W = X^T (X X^T + lam*s*I)^-1 Y, cheap when rows (anchors) are fewer than columns of X."""
    g = x @ x.T
    s = g.diagonal().mean()
    return x.T @ torch.linalg.solve(g + lam * s * torch.eye(x.shape[0], dtype=x.dtype), y)


def _ridge_primal(g: torch.Tensor, x: torch.Tensor, lam: float) -> torch.Tensor:
    """W = (G^T G + lam*s*I)^-1 G^T X for the small k x k system."""
    gg = g.T @ g
    s = gg.diagonal().mean()
    return torch.linalg.solve(gg + lam * s * torch.eye(g.shape[1], dtype=g.dtype), g.T @ x)


@dataclass
class Spoke:
    """One model's adapters. `mean` is the centre of its anchor states; differences of means need no centring."""
    mean: torch.Tensor
    enc: torch.Tensor  # d x k
    dec: torch.Tensor  # k x d
    heldout_cos: float = float("nan")


@dataclass
class Hub:
    k: int
    anchors: str  # sha256 of the anchor texts, so spokes are only combined if they were fitted on the same ones
    g: torch.Tensor  # n x k hub coordinates of the anchors
    spokes: dict[str, Spoke] = field(default_factory=dict)
    lam: float = 0.1

    @property
    def hub_id(self) -> str:
        return hashlib.sha256(json.dumps([self.k, self.anchors, self.g.shape[0]]).encode()).hexdigest()[:16]

    def encode(self, name: str, v: torch.Tensor) -> torch.Tensor:
        """A difference of means in `name`'s space to hub coordinates."""
        return v @ self.spokes[name].enc

    def decode(self, name: str, h: torch.Tensor) -> torch.Tensor:
        return h @ self.spokes[name].dec

    def send(self, src: str, dst: str, v: torch.Tensor) -> torch.Tensor:
        return self.decode(dst, self.encode(src, v))

    def add_spoke(self, name: str, states: torch.Tensor, heldout: tuple[torch.Tensor, torch.Tensor] | None = None) -> Spoke:
        """Fit `name` against the existing hub coordinates. `states` are its anchor states, rows aligned with `g`."""
        if states.shape[0] != self.g.shape[0]:
            raise ValueError(f"{states.shape[0]} anchor rows, hub was fitted on {self.g.shape[0]}")
        mean = states.mean(0)
        x = states - mean
        sp = Spoke(mean, _ridge_dual(x, self.g, self.lam), _ridge_primal(self.g, x, self.lam))
        if heldout is not None:
            hx, hg = heldout  # held-out anchor states of this model and the hub coordinates they should map to
            sp.heldout_cos = torch.nn.functional.cosine_similarity((hx - mean) @ sp.enc, hg, dim=-1).mean().item()
        self.spokes[name] = sp
        return sp


def anchor_digest(texts: list[str]) -> str:
    return hashlib.sha256("\n".join(texts).encode()).hexdigest()


def fit_hub(states: dict[str, torch.Tensor], anchors: str, k: int = 64, lam: float = 0.1, r: int | None = None) -> Hub:
    """MAXVAR GCCA over models' anchor states (each n x d_i, same row order), then one spoke per model."""
    n = next(iter(states.values())).shape[0]
    if any(s.shape[0] != n for s in states.values()):
        raise ValueError("every model needs a state row for every anchor")
    r = r or min(n - 1, 256)
    scores = []
    for s in states.values():
        x = (s - s.mean(0)).double()
        u, sv, _ = torch.linalg.svd(x, full_matrices=False)
        keep = int((sv > sv[0] * 1e-6).sum())
        scores.append(u[:, : min(r, keep)])  # whitened scores: orthonormal columns
    u, _, _ = torch.linalg.svd(torch.cat(scores, dim=1), full_matrices=False)
    k = min(k, u.shape[1])
    g = (u[:, :k] * (n ** 0.5)).float()  # unit variance per hub coordinate
    hub = Hub(k=k, anchors=anchors, g=g, lam=lam)
    for name, s in states.items():
        hub.add_spoke(name, s.float())
    return hub


def quantize(h: torch.Tensor) -> tuple[bytes, float]:
    """Symmetric int8 with one scale. k bytes carry the thought."""
    scale = float(h.abs().max().clamp_min(1e-12)) / 127.0
    q = torch.clamp((h / scale).round(), -127, 127).to(torch.int8)
    return bytes(q.numpy().tobytes()), scale


def dequantize(raw: bytes, scale: float) -> torch.Tensor:
    return torch.frombuffer(bytearray(raw), dtype=torch.int8).float() * scale


def pack(hub: Hub, src: str, h: torch.Tensor, extra: dict | None = None) -> bytes:
    """`PBL1` | u32 header length | JSON header | int8 body. The header names the hub and the sender, never the text."""
    raw, scale = quantize(h)
    head = {"hub": hub.hub_id, "src": src, "k": hub.k, "scale": scale, "body_sha256": hashlib.sha256(raw).hexdigest(),
            **(extra or {})}
    hb = json.dumps(head, sort_keys=True).encode()
    return MAGIC + struct.pack(">I", len(hb)) + hb + raw


def unpack(hub: Hub, blob: bytes, max_bytes: int = 1 << 16) -> tuple[dict, torch.Tensor]:
    """Refuses anything malformed, oversized, for another hub, or whose body does not match its header."""
    if len(blob) > max_bytes or blob[:4] != MAGIC or len(blob) < 8:
        raise ValueError("not a PBL1 message")
    (n,) = struct.unpack(">I", blob[4:8])
    if n > len(blob) - 8:
        raise ValueError("truncated header")
    head = json.loads(blob[8 : 8 + n])
    raw = blob[8 + n :]
    if head.get("hub") != hub.hub_id:
        raise ValueError("message is for a different hub")
    if head.get("k") != hub.k or len(raw) != hub.k:
        raise ValueError("body length does not match the hub")
    if hashlib.sha256(raw).hexdigest() != head.get("body_sha256"):
        raise ValueError("body does not match its header")
    scale = float(head["scale"])
    if not (scale > 0 and scale < 1e6):
        raise ValueError("bad scale")
    return head, dequantize(raw, scale)


# --- Signed envelope for a room (`PBL2`), checked by the Rust host (crates/playbox-net/src/latent.rs) --------------------

MAGIC2 = b"PBL2"


def _signed_text(hub: str, src: str, k: int, scale_f32: str, body_blake3: str, signer: str) -> bytes:
    return f"playbox.latent.v1\nhub {hub}\nsrc {src}\nk {k}\nscale {scale_f32}\nbody {body_blake3}\nsigner {signer}\n".encode()


def pack_signed(hub: Hub, src: str, h: torch.Tensor, private_key) -> bytes:
    """`PBL2` | u32 header length | JSON header | int8 body, signed by `private_key` (a cryptography Ed25519 key).

    In a room the key must be the member's own endpoint key: the host refuses a message signed by anyone else. The scale is
    carried as the hex of its big-endian float32 so no float text has to round-trip between Python and Rust, and the
    quantization uses that float32 scale, exactly what the receiver multiplies back by.
    """
    import blake3
    from cryptography.hazmat.primitives import serialization

    scale_hex = struct.pack(">f", float(h.abs().max().clamp_min(1e-12)) / 127.0).hex()
    scale = struct.unpack(">f", bytes.fromhex(scale_hex))[0]
    raw = bytes(torch.clamp((h / scale).round(), -127, 127).to(torch.int8).numpy().tobytes())
    signer = private_key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    body_hash = blake3.blake3(raw).hexdigest()
    sig = private_key.sign(_signed_text(hub.hub_id, src, hub.k, scale_hex, body_hash, signer)).hex()
    head = {"hub": hub.hub_id, "src": src, "k": hub.k, "scale_f32": scale_hex, "body_blake3": body_hash, "signer": signer, "sig": sig}
    hb = json.dumps(head, sort_keys=True, separators=(",", ":")).encode()
    return MAGIC2 + struct.pack(">I", len(hb)) + hb + raw


def unpack_signed(hub: Hub, blob: bytes, trusted_signers: set[str] | None = None, max_bytes: int = 1 << 16) -> tuple[dict, torch.Tensor]:
    """Refuses anything malformed, oversized, for another hub, unsigned, signed by a key not in `trusted_signers`
    (when given), or whose body does not match its header. Returns the header and the dequantized hub vector."""
    import blake3
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    if len(blob) > max_bytes or len(blob) < 8 or blob[:4] != MAGIC2:
        raise ValueError("not a PBL2 message")
    (n,) = struct.unpack(">I", blob[4:8])
    if n > 4096 or n > len(blob) - 8:
        raise ValueError("bad header length")
    head = json.loads(blob[8 : 8 + n])
    raw = blob[8 + n :]
    if head.get("hub") != hub.hub_id:
        raise ValueError("message is for a different hub")
    if head.get("k") != hub.k or len(raw) != hub.k:
        raise ValueError("body length does not match the hub")
    if blake3.blake3(raw).hexdigest() != head.get("body_blake3"):
        raise ValueError("body does not match its header")
    if trusted_signers is not None and head.get("signer") not in trusted_signers:
        raise ValueError("signer is not trusted")
    try:
        Ed25519PublicKey.from_public_bytes(bytes.fromhex(head["signer"])).verify(
            bytes.fromhex(head["sig"]),
            _signed_text(head["hub"], head["src"], head["k"], head["scale_f32"], head["body_blake3"], head["signer"]))
    except (InvalidSignature, ValueError, KeyError):
        raise ValueError("the signature is not valid") from None
    scale = struct.unpack(">f", bytes.fromhex(head["scale_f32"]))[0]
    if not (0 < scale < 1e6):
        raise ValueError("bad scale")
    return head, dequantize(raw, scale)
