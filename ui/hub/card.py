"""Signed agent cards (see crates/playbox-net/src/card.rs and docs/agent-identity-prd.md).

A card is `::pbx card <json> <signature hex>`: the member's ed25519 key signs `DOMAIN` + the JSON text exactly as sent. `verify` accepts it only
if the signature holds under the card's `key`, that key is the key the room says sent the message, the card is for this room, it has not
expired and was never valid for more than 24 hours. Pure Python (RFC 8032), so the hub needs nothing it does not already have."""
import hashlib
import json

DOMAIN = "playbox.card.v1\n"
PREFIX = "::pbx identity "
MAX_TTL_MS = 24 * 3600 * 1000
MAX_LEN = 2048

_P = 2**255 - 19
_L = 2**252 + 27742317777372353535851937790883648493
_D = -121665 * pow(121666, _P - 2, _P) % _P
_I = pow(2, (_P - 1) // 4, _P)


def _inv(x):
    return pow(x, _P - 2, _P)


def _xrecover(y):
    xx = (y * y - 1) * _inv(_D * y * y + 1)
    x = pow(xx, (_P + 3) // 8, _P)
    if (x * x - xx) % _P:
        x = x * _I % _P
    if (x * x - xx) % _P:
        raise ValueError("not on curve")
    return _P - x if x & 1 else x


_BY = 4 * _inv(5) % _P
_B = (_xrecover(_BY), _BY, 1, _xrecover(_BY) * _BY % _P)


def _add(p, q):
    a, b = (p[1] - p[0]) * (q[1] - q[0]) % _P, (p[1] + p[0]) * (q[1] + q[0]) % _P
    c, d = 2 * p[3] * q[3] * _D % _P, 2 * p[2] * q[2] % _P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _mul(s, p):
    q = (0, 1, 1, 0)
    while s:
        if s & 1:
            q = _add(q, p)
        p = _add(p, p)
        s >>= 1
    return q


def _enc(p):
    zi = _inv(p[2])
    x, y = p[0] * zi % _P, p[1] * zi % _P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def _dec(b):
    y = int.from_bytes(b, "little")
    sign, y = y >> 255, y & ((1 << 255) - 1)
    if y >= _P:
        raise ValueError("bad point")
    x = _xrecover(y)
    if x == 0 and sign == 1:
        raise ValueError("bad point")
    if x & 1 != sign:
        x = _P - x
    return (x, y, 1, x * y % _P)


def ed25519_verify(pub, msg, sig):
    """True if `sig` (64 bytes) is a valid ed25519 signature of `msg` under `pub` (32 bytes). Never raises."""
    try:
        if len(pub) != 32 or len(sig) != 64:
            return False
        a, r = _dec(pub), _dec(sig[:32])
        s = int.from_bytes(sig[32:], "little")
        if s >= _L:
            return False
        a8 = _mul(8, a)
        if a8[0] % _P == 0 and a8[1] % _P == a8[2] % _P:
            return False
        h = int.from_bytes(hashlib.sha512(sig[:32] + pub + msg).digest(), "little") % _L
        lhs, rhs = _mul(8 * s, _B), _add(_mul(8, r), _mul(h, a8))
        return lhs[0] * rhs[2] % _P == rhs[0] * lhs[2] % _P and lhs[1] * rhs[2] % _P == rhs[1] * lhs[2] % _P
    except (ValueError, ZeroDivisionError):
        return False


_A58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def did_key(pk):
    n = int.from_bytes(b"\xed\x01" + pk, "big")
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = _A58[r] + out
    return "did:key:z" + out


def _hex(s, n):
    if not isinstance(s, str) or len(s) != 2 * n or any(c not in "0123456789abcdef" for c in s):
        raise ValueError("bad hex")
    return bytes.fromhex(s)


def verify(text, sender, room, now_ms):
    """Return the card dict, or raise ValueError saying why it was refused."""
    if not isinstance(text, str) or len(text) > MAX_LEN:
        raise ValueError("too large")
    if not text.startswith(PREFIX):
        raise ValueError("not a card")
    body, sep, sig_hex = text[len(PREFIX):].rpartition(" ")
    if not sep:
        raise ValueError("malformed")
    sig = _hex(sig_hex, 64)
    try:
        card = json.loads(body)
    except ValueError:
        raise ValueError("bad json")
    if not isinstance(card, dict):
        raise ValueError("bad json")
    key_hex = card.get("key")
    key = _hex(key_hex, 32)
    if key_hex != sender:
        raise ValueError("key is not the sender")
    if not ed25519_verify(key, (DOMAIN + body).encode(), sig):
        raise ValueError("signature does not verify")
    if card.get("v") != 1:
        raise ValueError("unknown version")
    if not room or card.get("room") != room:
        raise ValueError("card is for another room")
    if card.get("did") != did_key(key):
        raise ValueError("did does not match key")
    issued, expires = card.get("issued_ms"), card.get("expires_ms")
    if not all(isinstance(x, int) and not isinstance(x, bool) for x in (issued, expires)):
        raise ValueError("no lifetime")
    if expires <= issued or expires - issued > MAX_TTL_MS:
        raise ValueError("lifetime over 24 hours")
    if now_ms >= expires:
        raise ValueError("expired")
    return card


LAUNCH_DOMAIN = "playbox.launch.v1\n"
LAUNCH_PREFIX = "::pbx launch "


def verify_launch(text, room):
    """A host-signed launch record, or ValueError. The room id is the host's key: only the host can have signed it."""
    if not isinstance(text, str) or len(text) > MAX_LEN:
        raise ValueError("too large")
    if not text.startswith(LAUNCH_PREFIX):
        raise ValueError("not a launch record")
    body, sep, sig_hex = text[len(LAUNCH_PREFIX):].rpartition(" ")
    if not sep:
        raise ValueError("malformed")
    sig = _hex(sig_hex, 64)
    try:
        rec = json.loads(body)
    except ValueError:
        raise ValueError("bad json")
    if not isinstance(rec, dict):
        raise ValueError("bad json")
    if not ed25519_verify(_hex(room, 32), (LAUNCH_DOMAIN + body).encode(), sig):
        raise ValueError("not signed by this room's host")
    if rec.get("v") != 1 or rec.get("room") != room:
        raise ValueError("record is for another room")
    _hex(rec.get("key"), 32)
    if not isinstance(rec.get("started_ms"), int):
        raise ValueError("no start time")
    return rec
