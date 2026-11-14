"""Passkeys (WebAuthn level 2, ES256 and RS256), verified in pure Python.

What is checked: challenge, type, origin host == RP ID, rpIdHash, user-present AND user-verified flags,
signature over authData || SHA-256(clientDataJSON), and the signature counter when the authenticator has one.
What is not: attestation statements. Registration asks for attestation "none" and a self-hosted hub trusts
the person holding its one-time pairing code, not a manufacturer certificate chain.
"""
import base64
import hashlib
import json
import struct
from urllib.parse import urlparse

from . import p256

ES256, RS256 = -7, -257
RSA_MIN_BITS, RSA_MAX_BITS = 2048, 8192
SHA256_PREFIX = bytes.fromhex("3031300d060960864801650304020105000420")   # DigestInfo for SHA-256 (RFC 8017)


def b64u(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def unb64u(s):
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


class WebAuthnError(ValueError):
    pass


def cbor_decode(buf, i=0, depth=0):
    """Minimal CBOR (RFC 8949): ints, bytes, text, arrays, maps, tags, simple values. Returns (value, next_index)."""
    if depth > 8 or i >= len(buf):
        raise WebAuthnError("bad CBOR")
    ib = buf[i]
    major, info = ib >> 5, ib & 31
    i += 1
    if info < 24:
        n = info
    elif info in (24, 25, 26, 27):
        ln = 1 << (info - 24)
        if i + ln > len(buf):
            raise WebAuthnError("bad CBOR")
        n = int.from_bytes(buf[i:i + ln], "big")
        i += ln
    else:
        raise WebAuthnError("unsupported CBOR")
    if major == 0:
        return n, i
    if major == 1:
        return -1 - n, i
    if major in (2, 3):
        if i + n > len(buf):
            raise WebAuthnError("bad CBOR")
        raw = bytes(buf[i:i + n])
        return (raw if major == 2 else raw.decode("utf-8")), i + n
    if major == 4:
        if n > 64:
            raise WebAuthnError("bad CBOR")
        out = []
        for _ in range(n):
            v, i = cbor_decode(buf, i, depth + 1)
            out.append(v)
        return out, i
    if major == 5:
        if n > 64:
            raise WebAuthnError("bad CBOR")
        out = {}
        for _ in range(n):
            k, i = cbor_decode(buf, i, depth + 1)
            v, i = cbor_decode(buf, i, depth + 1)
            out[k] = v
        return out, i
    if major == 6:
        return cbor_decode(buf, i, depth + 1)
    if major == 7 and info in (20, 21, 22):
        return {20: False, 21: True, 22: None}[info], i
    raise WebAuthnError("unsupported CBOR")


def rsa_pkcs1_sha256_verify(n, e, msg, sig):
    """RSASSA-PKCS1-v1_5 / SHA-256 (RFC 8017 8.2.2). Compares the whole padded block, so no Bleichenbacher-style forgeries."""
    k = (n.bit_length() + 7) // 8
    if len(sig) != k or int.from_bytes(sig, "big") >= n:
        return False
    em = pow(int.from_bytes(sig, "big"), e, n).to_bytes(k, "big")
    t = SHA256_PREFIX + hashlib.sha256(msg).digest()
    return em == b"\x00\x01" + b"\xff" * (k - len(t) - 3) + b"\x00" + t


def _rsa_key_ok(n, e):
    return RSA_MIN_BITS <= n.bit_length() <= RSA_MAX_BITS and n & 1 and e & 1 and 65537 <= e < 1 << 64


def _client_data(raw, ctype, challenge, rp_id):
    try:
        cd = json.loads(raw)
    except ValueError:
        raise WebAuthnError("bad clientDataJSON")
    if cd.get("type") != ctype:
        raise WebAuthnError("wrong ceremony type")
    try:
        if unb64u(cd.get("challenge", "")) != challenge:
            raise WebAuthnError("challenge mismatch")
    except (ValueError, TypeError):
        raise WebAuthnError("challenge mismatch")
    if cd.get("crossOrigin"):
        raise WebAuthnError("cross-origin ceremony refused")
    u = urlparse(cd.get("origin", ""))
    local = u.hostname in ("localhost", "127.0.0.1", "::1")
    if u.hostname != rp_id or not (u.scheme == "https" or (u.scheme == "http" and local)):
        raise WebAuthnError("origin does not match this hub")
    return cd


def _auth_data(ad, rp_id):
    if len(ad) < 37 or ad[:32] != hashlib.sha256(rp_id.encode()).digest():
        raise WebAuthnError("rpIdHash mismatch")
    flags = ad[32]
    if not flags & 1:
        raise WebAuthnError("user presence missing")
    if not flags & 4:
        raise WebAuthnError("user verification missing (biometric or PIN required)")
    return flags, struct.unpack(">I", ad[33:37])[0]


def verify_registration(resp, challenge, rp_id):
    """resp: {clientDataJSON, attestationObject} as base64url. Returns {id, x, y, count}."""
    try:
        cd_raw = unb64u(resp["clientDataJSON"])
        att, _ = cbor_decode(unb64u(resp["attestationObject"]))
        ad = att["authData"]
    except (KeyError, ValueError, TypeError):
        raise WebAuthnError("malformed registration")
    _client_data(cd_raw, "webauthn.create", challenge, rp_id)
    flags, count = _auth_data(ad, rp_id)
    if not flags & 64 or len(ad) < 55:
        raise WebAuthnError("no credential data")
    cid_len = struct.unpack(">H", ad[53:55])[0]
    cid = ad[55:55 + cid_len]
    if not 16 <= len(cid) <= 1023 or len(cid) != cid_len:
        raise WebAuthnError("bad credential id")
    key, _ = cbor_decode(ad, 55 + cid_len)
    if isinstance(key, dict) and key.get(1) == 3 and key.get(3) == RS256:
        nb, eb = key.get(-1), key.get(-2)
        if not (isinstance(nb, bytes) and isinstance(eb, bytes) and 0 < len(eb) <= 8 and nb[:1] != b"\0" and eb[:1] != b"\0"):
            raise WebAuthnError("bad public key")
        n, e = int.from_bytes(nb, "big"), int.from_bytes(eb, "big")
        if not _rsa_key_ok(n, e):
            raise WebAuthnError("RSA key too small or exponent unsupported (need >= 2048 bits, e >= 65537, odd)")
        return {"id": b64u(cid), "alg": RS256, "n": nb.hex(), "e": eb.hex(), "count": count}
    if not isinstance(key, dict) or key.get(1) != 2 or key.get(3) != ES256 or key.get(-1) != 1:
        raise WebAuthnError("only ES256 (P-256) and RS256 passkeys are supported")
    x, y = key.get(-2), key.get(-3)
    if not (isinstance(x, bytes) and isinstance(y, bytes) and len(x) == len(y) == 32):
        raise WebAuthnError("bad public key")
    pt = (int.from_bytes(x, "big"), int.from_bytes(y, "big"))
    if not p256.on_curve(pt):
        raise WebAuthnError("public key not on curve")
    return {"id": b64u(cid), "x": x.hex(), "y": y.hex(), "count": count}


def verify_assertion(resp, challenge, rp_id, cred):
    """resp: {clientDataJSON, authenticatorData, signature}. cred: stored {x, y, count}. Returns the new counter."""
    try:
        cd_raw = unb64u(resp["clientDataJSON"])
        ad = unb64u(resp["authenticatorData"])
        sig = unb64u(resp["signature"])
    except (KeyError, ValueError, TypeError):
        raise WebAuthnError("malformed assertion")
    _client_data(cd_raw, "webauthn.get", challenge, rp_id)
    _, count = _auth_data(ad, rp_id)
    signed = ad + hashlib.sha256(cd_raw).digest()
    if cred.get("alg") == RS256:
        if not rsa_pkcs1_sha256_verify(int.from_bytes(bytes.fromhex(cred["n"]), "big"), int.from_bytes(bytes.fromhex(cred["e"]), "big"), signed, sig):
            raise WebAuthnError("bad signature")
    else:
        pub = (int.from_bytes(bytes.fromhex(cred["x"]), "big"), int.from_bytes(bytes.fromhex(cred["y"]), "big"))
        try:
            r, s = p256.der_decode(sig)
        except ValueError:
            raise WebAuthnError("bad signature")
        if not p256.verify_hash(pub, hashlib.sha256(signed).digest(), r, s):
            raise WebAuthnError("bad signature")
    if (count or cred.get("count")) and count <= cred.get("count", 0):
        raise WebAuthnError("signature counter went backwards (cloned authenticator?)")
    return count
