"""Cryptographically signed Playbox Attested Gate Receipt (v1).

Binds execution isolation, anti-reward-hacking git isolation proof, host audit log verification,
BOX-15/witness taint counts, AtlasKV provenance stats, router cascade provenance, and sealed-gate
verdict into one domain-separated hybrid post-quantum (`Ed25519+ML-DSA-65`, FIPS 204) JSON receipt.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path

RECEIPT_SCHEMA = "playbox.attested_gate_receipt.v1"
RECEIPT_DOMAIN = "playbox.attested_gate_receipt.v1"
RECEIPT_PQ_DOMAIN = "playbox.attested_gate_receipt.pq.v1"
MLDSA65_PK_HEX_LEN = 1952 * 2
MLDSA65_SIG_HEX_LEN = 3309 * 2


def _find_playbox_bin() -> str | None:
    """Locate the `playbox` CLI binary for FIPS 204 ML-DSA-65 hybrid receipt signing/verification."""
    candidates = [
        os.environ.get("PLAYBOX_BIN", ""),
        str(Path(__file__).resolve().parents[2] / "target" / "release" / "playbox"),
        str(Path(__file__).resolve().parents[2] / "target" / "debug" / "playbox"),
        os.path.expanduser("~/.local/bin/playbox"),
        os.path.expanduser("~/.cargo/bin/playbox"),
        shutil.which("playbox") or "",
    ]
    for c in candidates:
        if c and os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return None


def get_playbox_tool_version() -> str:
    """Return the exact `playbox --version` string (e.g. `playbox 0.4.4 (a1b2c3d4e)`)."""
    pb = _find_playbox_bin()
    if not pb:
        return "unknown"
    try:
        out = subprocess.check_output([pb, "--version"], text=True, stderr=subprocess.DEVNULL, timeout=5).strip()
        return out or "unknown"
    except (OSError, subprocess.SubprocessError):
        return "unknown"


def canonical_json_bytes(obj: dict) -> bytes:
    """Deterministic UTF-8 canonical JSON encoding for hashing and signing."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")


def sha256_file(path: str | Path | None) -> str:
    if not path:
        return ""
    p = Path(path)
    if not p.is_file():
        return ""
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def verify_single_commit_repo(repo_dir: str | Path) -> tuple[bool, int]:
    """Empirically verify that `repo_dir` contains at most 1 commit across all refs (`--all`).

    Closes the SWE-bench `git log --all` future-commit reward-hacking vector by construction.
    """
    p = Path(repo_dir)
    if not (p / ".git").exists():
        return True, 0
    try:
        out = subprocess.check_output(
            ["git", "-c", "core.hooksPath=/dev/null", "-C", str(p), "rev-list", "--all", "--count"],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
        cnt = int(out)
        return (cnt <= 1), cnt
    except (OSError, subprocess.SubprocessError, ValueError):
        return False, -1


def load_or_create_signing_key(key_path: str | Path | None = None):
    """Load or generate a 32-byte Ed25519 signing key (`0o600` when persisted)."""
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    if key_path is None:
        return Ed25519PrivateKey.generate()
    p = Path(key_path)
    if p.exists():
        raw = bytes.fromhex(p.read_text(encoding="utf-8").strip())
        if len(raw) != 32:
            raise ValueError(f"invalid Ed25519 seed length in {p}: {len(raw)}")
        return Ed25519PrivateKey.from_private_bytes(raw)
    p.parent.mkdir(parents=True, exist_ok=True)
    seed = os.urandom(32)
    tmp = p.parent / f".{p.name}.tmp.{os.getpid()}"
    tmp.write_text(seed.hex() + "\n", encoding="utf-8")
    os.chmod(tmp, 0o600)
    if not p.exists():
        try:
            os.link(tmp, p)
        except FileExistsError:
            pass
    try:
        tmp.unlink()
    except OSError:
        pass
    raw = bytes.fromhex(p.read_text(encoding="utf-8").strip())
    return Ed25519PrivateKey.from_private_bytes(raw)


def _signing_message(payload_sha256: str) -> bytes:
    return f"{RECEIPT_DOMAIN}\n{payload_sha256}".encode("ascii")


def build_signed_receipt(
    *,
    instance: str,
    base_commit: str,
    runtime: str,
    git_history_isolated: bool,
    repo_commit_count: int = 1,
    policy_digest: str = "",
    network_isolated: bool = True,
    box7_egress_hosts: list[str] | None = None,
    host_audit_verified: bool = False,
    audit_log_path: str | Path | None = None,
    taint_summary: dict | None = None,
    atlas_summary: dict | None = None,
    router_provenance: dict | None = None,
    sealed_gate: dict | None = None,
    llm_ledger: dict | None = None,
    signing_key=None,
) -> dict:
    """Assemble and Ed25519-sign (+ hybrid ML-DSA-65 when `playbox` CLI is available) a Playbox Attested Gate Receipt.

    When `llm_ledger` is provided (a verified `playbox.llm_ledger.v1` object from host `playbox-aiproxy`,
    or an already-summarized `host_aiproxy` ledger block), its canonical SHA-256 digest and host-observed
    token/cost totals are bound inside the signed payload. Otherwise, `llm_ledger.source` is explicitly
    recorded as `"client_unverified"`.
    """
    from .aiproxy import LEDGER_SCHEMA, summarize_ledger_for_receipt

    if llm_ledger is None:
        ledger_block: dict = {"source": "client_unverified"}
    elif llm_ledger.get("schema") == LEDGER_SCHEMA and "entries" in llm_ledger:
        ledger_block = summarize_ledger_for_receipt(llm_ledger)
    else:
        ledger_block = dict(llm_ledger)

    tool_ver = get_playbox_tool_version()
    payload = {
        "schema": RECEIPT_SCHEMA,
        "issued_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "tool_version": tool_ver,
        "git_dirty": "+dirty" in tool_ver,
        "instance": instance,
        "base_commit": base_commit,
        "runtime": runtime,
        "isolation": {
            "git_history_isolated": bool(git_history_isolated),
            "repo_commit_count": int(repo_commit_count),
            "guest_git_dir_stripped": True,
            "network_isolated": bool(network_isolated),
            "policy_digest": policy_digest,
            "box7_egress_hosts": list(box7_egress_hosts or []),
        },
        "audit": {
            "host_audit_verified": bool(host_audit_verified),
            "audit_log_sha256": sha256_file(audit_log_path),
        },
        "taint": dict(taint_summary or {}),
        "atlas": dict(atlas_summary or {}),
        "router": dict(router_provenance or {}),
        "sealed_gate": dict(sealed_gate or {}),
        "llm_ledger": ledger_block,
    }
    payload_sha256 = hashlib.sha256(canonical_json_bytes(payload)).hexdigest()

    sig_block: dict = {"alg": "none", "domain": RECEIPT_DOMAIN, "signed": False}
    seed_bytes: bytes | None = None
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

        if isinstance(signing_key, (str, Path)):
            sk = load_or_create_signing_key(signing_key)
        elif isinstance(signing_key, bytes) and len(signing_key) == 32:
            sk = Ed25519PrivateKey.from_private_bytes(signing_key)
        elif isinstance(signing_key, Ed25519PrivateKey):
            sk = signing_key
        else:
            sk = Ed25519PrivateKey.generate()
        seed_bytes = sk.private_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PrivateFormat.Raw,
            encryption_algorithm=serialization.NoEncryption(),
        )
        pk_bytes = sk.public_key().public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        )
        sig_bytes = sk.sign(_signing_message(payload_sha256))
        sig_block = {
            "alg": "Ed25519",
            "domain": RECEIPT_DOMAIN,
            "signed": True,
            "pubkey_hex": pk_bytes.hex(),
            "sig_hex": sig_bytes.hex(),
        }
    except ImportError:
        pass

    doc = {
        "payload": payload,
        "payload_sha256": payload_sha256,
        "signature": sig_block,
    }

    # Upgrade to hybrid post-quantum `Ed25519+ML-DSA-65` (FIPS 204) via `playbox receipt sign` when available.
    pb_bin = _find_playbox_bin()
    if pb_bin and seed_bytes is not None:
        with tempfile.TemporaryDirectory(prefix="pbx-rcpt-") as td:
            key_file = Path(td, "seed.key")
            rcpt_file = Path(td, "receipt.json")
            key_file.write_text(seed_bytes.hex() + "\n", encoding="utf-8")
            os.chmod(key_file, 0o600)
            rcpt_file.write_text(json.dumps(doc), encoding="utf-8")
            proc = subprocess.run(
                [pb_bin, "receipt", "sign", str(rcpt_file), "--key", str(key_file)],
                capture_output=True,
                text=True,
                timeout=15,
            )
            if proc.returncode == 0:
                try:
                    return json.loads(rcpt_file.read_text(encoding="utf-8"))
                except (OSError, ValueError):
                    pass

    return doc


def verify_signed_receipt(
    receipt: dict,
    *,
    expected_pubkey_hex: str | None = None,
    expected_pq_pubkey_hex: str | None = None,
    require_pq: bool = False,
) -> tuple[bool, str]:
    """Verify canonical payload hash and domain-separated Ed25519 (+ optional/required ML-DSA-65) signature."""
    if not isinstance(receipt, dict) or "payload" not in receipt or "signature" not in receipt:
        return False, "malformed receipt structure"
    payload = receipt["payload"]
    if not isinstance(payload, dict) or payload.get("schema") != RECEIPT_SCHEMA:
        return False, f"unexpected schema: {payload.get('schema') if isinstance(payload, dict) else None}"
    computed_sha = hashlib.sha256(canonical_json_bytes(payload)).hexdigest()
    claimed_sha = receipt.get("payload_sha256", "")
    if computed_sha != claimed_sha:
        return False, "payload_sha256 mismatch (receipt modified)"

    sig = receipt["signature"]
    if not isinstance(sig, dict) or not sig.get("signed"):
        return False, "receipt is not signed"
    alg = str(sig.get("alg") or "")
    if alg not in ("Ed25519", "Ed25519+ML-DSA-65"):
        return False, f"unsupported receipt signature alg: {alg!r}"
    if sig.get("domain") != RECEIPT_DOMAIN:
        return False, f"unexpected signature domain: {sig.get('domain')}"
    pk_hex = str(sig.get("pubkey_hex") or "")
    sig_hex = str(sig.get("sig_hex") or "")
    if expected_pubkey_hex and pk_hex.lower() != expected_pubkey_hex.lower():
        return False, "signer pubkey does not match expected_pubkey_hex"
    try:
        from cryptography.exceptions import InvalidSignature
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

        pk = Ed25519PublicKey.from_public_bytes(bytes.fromhex(pk_hex))
        pk.verify(bytes.fromhex(sig_hex), _signing_message(computed_sha))
    except (ValueError, InvalidSignature) as exc:
        return False, f"signature verification failed: {exc}"
    except ImportError:
        return False, "cryptography package unavailable to verify Ed25519 signature"

    has_pq = alg == "Ed25519+ML-DSA-65" or bool(sig.get("pq_sig_hex"))
    if require_pq and not has_pq:
        return False, "receipt is missing required ML-DSA-65 post-quantum signature"
    if has_pq:
        if sig.get("pq_alg") != "ML-DSA-65":
            return False, f"unexpected pq_alg: {sig.get('pq_alg')!r}"
        if sig.get("pq_domain") != RECEIPT_PQ_DOMAIN:
            return False, f"unexpected pq_domain: {sig.get('pq_domain')!r}"
        pq_pk_hex = str(sig.get("pq_pubkey_hex") or "")
        pq_sig_hex = str(sig.get("pq_sig_hex") or "")
        if len(pq_pk_hex) != MLDSA65_PK_HEX_LEN or len(pq_sig_hex) != MLDSA65_SIG_HEX_LEN:
            return False, "malformed ML-DSA-65 public key or signature length"
        try:
            bytes.fromhex(pq_pk_hex)
            bytes.fromhex(pq_sig_hex)
        except ValueError as exc:
            return False, f"invalid ML-DSA-65 hex encoding: {exc}"
        if expected_pq_pubkey_hex and pq_pk_hex.lower() != expected_pq_pubkey_hex.lower():
            return False, "signer ML-DSA-65 pubkey does not match expected_pq_pubkey_hex"

        pb_bin = _find_playbox_bin()
        if pb_bin:
            with tempfile.TemporaryDirectory(prefix="pbx-rcpt-vfy-") as td:
                rcpt_file = Path(td, "receipt.json")
                rcpt_file.write_text(json.dumps(receipt), encoding="utf-8")
                cmd = [pb_bin, "receipt", "verify", str(rcpt_file), "--require-pq"]
                if expected_pubkey_hex:
                    cmd.extend(["--pubkey", expected_pubkey_hex])
                if expected_pq_pubkey_hex:
                    cmd.extend(["--pq-pubkey", expected_pq_pubkey_hex])
                proc = subprocess.run(cmd, capture_output=True, text=True, timeout=15)
                if proc.returncode != 0:
                    err = (proc.stderr or proc.stdout or "ML-DSA-65 verification failed").strip()
                    return False, f"hybrid PQ verification failed: {err}"
                return True, "verified (Ed25519+ML-DSA-65)"
        return True, "verified (Ed25519; ML-DSA-65 structural)"

    return True, "verified"


def verify_receipt_ledger(receipt: dict, ledger: dict) -> tuple[bool, str]:
    """Verify that an external `playbox.llm_ledger.v1` file matches the `ledger_sha256` bound in `receipt`."""
    from .aiproxy import verify_ledger

    ok_l, why_l = verify_ledger(ledger)
    if not ok_l:
        return False, f"ledger invalid: {why_l}"
    payload = receipt.get("payload") if isinstance(receipt, dict) else None
    bound = payload.get("llm_ledger") if isinstance(payload, dict) else None
    if not isinstance(bound, dict) or bound.get("source") != "host_aiproxy":
        return False, "receipt does not bind a host_aiproxy llm_ledger"
    bound_sha = str(bound.get("ledger_sha256") or "").lower()
    ledger_sha = str(ledger.get("ledger_sha256") or "").lower()
    if not bound_sha or bound_sha != ledger_sha:
        return False, "receipt llm_ledger.ledger_sha256 does not match ledger"
    return True, "ledger matches signed receipt"
