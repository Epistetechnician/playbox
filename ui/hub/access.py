"""Cloudflare Access: verify the signed token Access puts on every request it lets through (stdlib only).

Cloudflare Access sits in front of the tunnel and adds `Cf-Access-Jwt-Assertion`, an RS256 JWT. The hub trusts that token, not the
network path: a request that reaches the tunnel without passing Access (a misconfigured route, a second hostname) carries no valid
token and is refused. The token's audience must be this application's AUD tag and its issuer your team domain."""
import base64
import hashlib
import json
import os
import threading
import time
import urllib.request

# DER prefix of DigestInfo for SHA-256 (RFC 8017, EMSA-PKCS1-v1_5)
SHA256_PREFIX = bytes.fromhex("3031300d060960864801650304020105000420")


def _unb64u(s):
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


class AccessError(Exception):
    pass


def rsa_verify(n, e, signed, sig):
    k = (n.bit_length() + 7) // 8
    if len(sig) != k or int.from_bytes(sig, "big") >= n:     # s >= n would make s and s-n both verify
        raise AccessError("bad signature length")
    em = pow(int.from_bytes(sig, "big"), e, n).to_bytes(k, "big")
    t = SHA256_PREFIX + hashlib.sha256(signed).digest()
    expect = b"\x00\x01" + b"\xff" * (k - len(t) - 3) + b"\x00" + t
    if em != expect:     # exact comparison of the whole padded block, which also rejects Bleichenbacher-style forgeries
        raise AccessError("bad signature")


class Access:
    def __init__(self, team, aud, keys_file=None):
        team = team.strip().lower().removeprefix("https://").rstrip("/")
        self.host = team if "." in team else f"{team}.cloudflareaccess.com"
        self.iss = f"https://{self.host}"
        self.aud = aud.strip()
        self.keys_file = keys_file or os.environ.get("PLAYBOX_HUB_ACCESS_KEYS")     # tests: a local JWKS file instead of the network
        self.keys, self.fetched, self.lock = {}, 0.0, threading.Lock()

    def _load(self, force=False):
        with self.lock:
            if not force and self.keys and time.time() - self.fetched < 3600:
                return
            if self.keys and time.time() - self.fetched < 60:     # unknown kid: refetch at most once a minute
                return
            if self.keys_file:
                doc = json.load(open(self.keys_file))
            else:
                with urllib.request.urlopen(f"{self.iss}/cdn-cgi/access/certs", timeout=8) as r:
                    doc = json.loads(r.read(1 << 20))
            self.keys = {k["kid"]: (int.from_bytes(_unb64u(k["n"]), "big"), int.from_bytes(_unb64u(k["e"]), "big")) for k in doc.get("keys", []) if k.get("kty") == "RSA"}
            self.fetched = time.time()

    def verify(self, token, now=None):
        """Returns the claims, or raises AccessError."""
        try:
            h64, p64, s64 = token.split(".")
            head, claims, sig = json.loads(_unb64u(h64)), json.loads(_unb64u(p64)), _unb64u(s64)
        except (ValueError, AttributeError):
            raise AccessError("malformed token")
        if head.get("alg") != "RS256":
            raise AccessError("unexpected algorithm")      # never accept "none" or a symmetric alg
        try:
            self._load()
            if head.get("kid") not in self.keys:
                self._load(force=True)
        except Exception as e:
            raise AccessError(f"cannot load Access keys: {e}")
        key = self.keys.get(head.get("kid"))
        if not key:
            raise AccessError("unknown signing key")
        rsa_verify(key[0], key[1], f"{h64}.{p64}".encode(), sig)
        now = now or time.time()
        if claims.get("iss") != self.iss:
            raise AccessError("wrong issuer")
        aud = claims.get("aud")
        if self.aud not in (aud if isinstance(aud, list) else [aud]):
            raise AccessError("wrong audience")
        if not isinstance(claims.get("exp"), (int, float)) or claims["exp"] < now - 30:
            raise AccessError("token expired")
        if isinstance(claims.get("nbf"), (int, float)) and claims["nbf"] > now + 30:
            raise AccessError("token not yet valid")
        return claims
