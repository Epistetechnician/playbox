"""Shared pieces: hashing, host key, hash-chained signed receipts."""
from __future__ import annotations

import hashlib
import json
import platform
import time
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.hazmat.primitives import serialization

ROOT = Path(__file__).resolve().parent
STATE = ROOT / "state"


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def canon(obj) -> bytes:
    return json.dumps(obj, sort_keys=True, separators=(",", ":")).encode()


def host_key() -> Ed25519PrivateKey:
    STATE.mkdir(exist_ok=True)
    kp = STATE / "host.key"
    if kp.exists():
        return serialization.load_pem_private_key(kp.read_bytes(), None)
    k = Ed25519PrivateKey.generate()
    kp.write_bytes(k.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    return k


def pubkey_hex(k: Ed25519PrivateKey) -> str:
    return k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()


def hardware() -> dict:
    # Self-reported fingerprint. A real room binds this to a Secure Enclave / TPM / TDX attestation (docs/attestation.md).
    return {"machine": platform.machine(), "system": platform.platform(), "python": platform.python_version(), "attested": False}


def append_receipt(log: Path, body: dict) -> dict:
    k = host_key()
    prev = "0" * 64
    if log.exists() and log.read_text().strip():
        prev = json.loads(log.read_text().strip().splitlines()[-1])["id"]
    body = {**body, "prev": prev, "ts": int(time.time()), "host": pubkey_hex(k), "hardware": hardware()}
    rid = sha256_bytes(canon(body))
    rec = {"id": rid, "body": body, "sig": k.sign(rid.encode()).hex()}
    with open(log, "a") as f:
        f.write(json.dumps(rec) + "\n")
    return rec


def verify_log(log: Path) -> int:
    n, prev = 0, "0" * 64
    for line in log.read_text().strip().splitlines():
        rec = json.loads(line)
        b = rec["body"]
        assert b["prev"] == prev, f"chain broken at {n}"
        assert sha256_bytes(canon(b)) == rec["id"], f"hash mismatch at {n}"
        Ed25519PublicKey.from_public_bytes(bytes.fromhex(b["host"])).verify(bytes.fromhex(rec["sig"]), rec["id"].encode())
        prev = rec["id"]
        n += 1
    return n
