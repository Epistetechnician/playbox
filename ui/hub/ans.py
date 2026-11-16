"""Check an Agent Name Service (ANS) receipt offline, with the standard library only.

An ANS receipt is a SCITT COSE_Sign1 (CBOR tag 18) signed with ES256 by the transparency log's one P-256 key. The payload is the logged event
(JSON); its `producer.event.ansName` is the name. The Merkle inclusion proof rides in the unprotected header (label 396) and is NOT covered by the
signature, so it is checked only for consistency with itself.

What a good check proves: the log whose key *you pinned* signed an event for that name. It does not prove that the room member's key is the name's
holder: the receipt carries no member key. The hub therefore shows "registered", never "this agent is that name". Formats are those of
https://github.com/agentnameservice/ans (internal/tl/receipt); nothing here talks to a network."""
import base64
import hashlib
import json
import re

P = 0xFFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF
N = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551
B = 0x5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B
G = (0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296, 0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5)
MAX_RECEIPT = 16384
SPKI_P256 = bytes.fromhex("3059301306072a8648ce3d020106082a8648ce3d030107034200")      # SubjectPublicKeyInfo header for an uncompressed P-256 point


def _inv(x, m):
    return pow(x, -1, m)


def _add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    (x1, y1), (x2, y2) = a, b
    if x1 == x2:
        if (y1 + y2) % P == 0:
            return None
        s = (3 * x1 * x1 - 3) * _inv(2 * y1, P) % P
    else:
        s = (y2 - y1) * _inv(x2 - x1, P) % P
    x3 = (s * s - x1 - x2) % P
    return x3, (s * (x1 - x3) - y1) % P


def _mul(k, pt):
    r = None
    while k:
        if k & 1:
            r = _add(r, pt)
        pt = _add(pt, pt)
        k >>= 1
    return r


def ecdsa_verify(pub, digest, r, s):
    """pub: (x, y); digest: 32 bytes; r, s: ints. True only for a valid P-256 signature."""
    x, y = pub
    if (y * y - (x * x * x - 3 * x + B)) % P != 0 or not (1 <= r < N and 1 <= s < N):
        return False
    w = _inv(s, N)
    pt = _add(_mul(int.from_bytes(digest, "big") * w % N, G), _mul(r * w % N, pub))
    return pt is not None and pt[0] % N == r


def load_key(text):
    """A pinned log key: PEM SPKI, base64 SPKI DER, or an ANS /root-keys line (`origin+hash+base64(0x02 || SPKI)`). Returns (x, y)."""
    t = text.strip()
    if "+" in t and "\n" not in t and not t.startswith("-----") and t.count("+") >= 2:
        t = t.rsplit("+", 1)[1]
        raw = base64.b64decode(t + "=" * (-len(t) % 4))
        if raw[:1] != b"\x02":
            raise ValueError("unrecognised root-key line")
        raw = raw[1:]
    else:
        body = "".join(l for l in t.splitlines() if not l.startswith("-----"))
        try:
            raw = base64.b64decode(body + "=" * (-len(body) % 4), validate=False)
        except ValueError:
            raise ValueError("the key is not base64")
    if not raw.startswith(SPKI_P256) or len(raw) != len(SPKI_P256) + 65 or raw[len(SPKI_P256) - 1:len(SPKI_P256)] != b"\x00":
        raise ValueError("the key is not a P-256 public key")
    pt = raw[len(SPKI_P256):]
    if pt[0] != 4:
        raise ValueError("the key is not an uncompressed P-256 point")
    pub = (int.from_bytes(pt[1:33], "big"), int.from_bytes(pt[33:], "big"))
    if (pub[1] ** 2 - (pub[0] ** 3 - 3 * pub[0] + B)) % P:
        raise ValueError("the key is not on the P-256 curve")
    return pub, raw


def _cbor(b, i=0, depth=0):
    """Decode one CBOR item at b[i:]; return (value, next index). Handles what a receipt uses; refuses indefinite lengths."""
    if depth > 8 or i >= len(b):
        raise ValueError("malformed receipt")
    ib = b[i]
    mt, ai = ib >> 5, ib & 31
    i += 1
    if ai < 24:
        n = ai
    elif ai in (24, 25, 26, 27):
        w = 1 << (ai - 24)
        if i + w > len(b):
            raise ValueError("malformed receipt")
        n = int.from_bytes(b[i:i + w], "big")
        i += w
    else:
        raise ValueError("unsupported CBOR")
    if mt == 0:
        return n, i
    if mt == 1:
        return -1 - n, i
    if mt in (2, 3):
        if i + n > len(b):
            raise ValueError("malformed receipt")
        v = bytes(b[i:i + n])
        return (v if mt == 2 else v.decode("utf-8", "replace")), i + n
    if mt == 4:
        if n > 256:
            raise ValueError("malformed receipt")
        out = []
        for _ in range(n):
            v, i = _cbor(b, i, depth + 1)
            out.append(v)
        return out, i
    if mt == 5:
        if n > 64:
            raise ValueError("malformed receipt")
        out = {}
        for _ in range(n):
            k, i = _cbor(b, i, depth + 1)
            v, i = _cbor(b, i, depth + 1)
            out[k if not isinstance(k, (list, dict)) else repr(k)] = v
        return out, i
    if mt == 6:
        v, i = _cbor(b, i, depth + 1)
        return ("tag", n, v), i
    raise ValueError("unsupported CBOR")


def _bstr(x):
    n = len(x)
    head = bytes([0x40 | n]) if n < 24 else bytes([0x58, n]) if n < 256 else bytes([0x59]) + n.to_bytes(2, "big") if n < 65536 else bytes([0x5A]) + n.to_bytes(4, "big")
    return head + x


def merkle_root(leaf_index, tree_size, path, leaf):
    """RFC 9162 section 2.1.3.2: the root an inclusion proof implies. Raises ValueError for a proof that cannot be right."""
    if tree_size == 0 or leaf_index >= tree_size or any(len(p) != 32 for p in path):
        raise ValueError("inconsistent inclusion proof")
    fn, sn, r = leaf_index, tree_size - 1, hashlib.sha256(b"\x00" + leaf).digest()
    for p in path:
        if sn == 0:
            raise ValueError("inclusion proof path too long")
        if fn & 1 or fn == sn:
            r = hashlib.sha256(b"\x01" + p + r).digest()
            while not fn & 1 and fn:
                fn >>= 1
                sn >>= 1
        else:
            r = hashlib.sha256(b"\x01" + r + p).digest()
        fn >>= 1
        sn >>= 1
    if sn != 0:
        raise ValueError("inclusion proof path too short")
    return r


def verify(receipt_b64, key_text, want_name):
    """Check a receipt for `want_name` against the pinned log key. Returns {name, leaf_hash, root_hash, kid, issuer, issued} or raises ValueError."""
    try:
        t = "".join(receipt_b64.split()).replace("-", "+").replace("_", "/")
        raw = base64.b64decode(t + "=" * (-len(t) % 4), validate=True)
    except ValueError:
        raise ValueError("the receipt is not base64")
    if not raw or len(raw) > MAX_RECEIPT:
        raise ValueError("the receipt is empty or too large")
    pub, spki = load_key(key_text)
    top, end = _cbor(raw)
    if end != len(raw):
        raise ValueError("trailing bytes after the receipt")
    if isinstance(top, tuple) and top[:2] == ("tag", 18):
        top = top[2]
    if not isinstance(top, list) or len(top) != 4 or not isinstance(top[0], bytes) or not isinstance(top[1], dict) or not isinstance(top[2], bytes) or not isinstance(top[3], bytes):
        raise ValueError("not a COSE_Sign1 receipt")
    prot_b, unprot, payload, sig = top
    prot, e = _cbor(prot_b)
    if not isinstance(prot, dict) or prot.get(1) != -7:
        raise ValueError("the receipt is not ES256")
    kid = prot.get(4)
    if isinstance(kid, bytes) and kid != hashlib.sha256(spki).digest()[:4]:
        raise ValueError("the receipt was signed by a different log key than the one pinned")
    if len(sig) != 64:
        raise ValueError("bad signature length")
    sig_structure = b"\x84" + b"\x6a" + b"Signature1" + _bstr(prot_b) + b"\x40" + _bstr(payload)
    if not ecdsa_verify(pub, hashlib.sha256(sig_structure).digest(), int.from_bytes(sig[:32], "big"), int.from_bytes(sig[32:], "big")):
        raise ValueError("the signature does not verify under the pinned log key")
    try:
        event = json.loads(payload.decode("utf-8"))
        name = event["producer"]["event"]["ansName"]
    except (ValueError, KeyError, TypeError):
        raise ValueError("the receipt has no event with an ansName")
    if not isinstance(name, str) or name != want_name:
        raise ValueError("the receipt is for a different name")
    vdp = unprot.get(396)
    if not isinstance(vdp, dict):
        raise ValueError("the receipt has no inclusion proof")
    size, idx, path, root = vdp.get(-1), vdp.get(-2), vdp.get(-3), vdp.get(-4)
    if not (isinstance(size, int) and isinstance(idx, int) and isinstance(path, list) and all(isinstance(x, bytes) for x in path) and isinstance(root, bytes)):
        raise ValueError("the inclusion proof is malformed")
    if merkle_root(idx, size, path, payload) != root:
        raise ValueError("the inclusion proof does not lead to its own root")
    cwt = prot.get(15) if isinstance(prot.get(15), dict) else {}
    return {"name": name, "leaf_hash": hashlib.sha256(b"\x00" + payload).hexdigest(), "root_hash": root.hex(), "tree_size": size,
            "kid": kid.hex() if isinstance(kid, bytes) else "", "issuer": str(cwt.get(1, ""))[:80], "issued": cwt.get(6) if isinstance(cwt.get(6), int) else None}


def name_ok(s):
    return isinstance(s, str) and 0 < len(s) <= 120 and re.fullmatch(r"[A-Za-z0-9:/._@+-]+", s) is not None
