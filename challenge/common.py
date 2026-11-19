"""Shared pieces: hashing, host key, hash-chained signed receipts."""
from __future__ import annotations

import fcntl
import hashlib
import json
import os
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
    pem = k.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption())
    try:
        fd = os.open(kp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        return serialization.load_pem_private_key(kp.read_bytes(), None)
    try:
        os.write(fd, pem)
    finally:
        os.close(fd)
    return k


def pubkey_hex(k: Ed25519PrivateKey) -> str:
    return k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()


def hardware() -> dict:
    # Self-reported fingerprint. A real room binds this to a Secure Enclave / TPM / TDX attestation (docs/attestation.md).
    return {"machine": platform.machine(), "system": platform.platform(), "python": platform.python_version(), "attested": False}


def append_receipt(log: Path, body: dict, key: Ed25519PrivateKey | None = None) -> dict:
    k = key or host_key()
    log.parent.mkdir(parents=True, exist_ok=True)
    with open(log, "a+") as f:
        fcntl.flock(f.fileno(), fcntl.LOCK_EX)
        try:
            f.seek(0)
            text = f.read().strip()
            prev = json.loads(text.splitlines()[-1])["id"] if text else "0" * 64
            full = {**body, "prev": prev, "ts": int(time.time()), "host": pubkey_hex(k), "hardware": hardware()}
            rid = sha256_bytes(canon(full))
            rec = {"id": rid, "body": full, "sig": k.sign(rid.encode()).hex()}
            f.seek(0, os.SEEK_END)
            f.write(json.dumps(rec) + "\n")
            f.flush()
        finally:
            fcntl.flock(f.fileno(), fcntl.LOCK_UN)
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
