"""Authentication for the hub.

Root of trust: whoever can read the hub's terminal (or its state directory) can mint a one-time pairing code.
A code lets a device either
  - enroll a passkey (WebAuthn, user verification required)  -> a "strong" session, or
  - open a "weak" session, only on a private network over plain HTTP where browsers will not run WebAuthn.
Every later visit is a passkey assertion. Sessions are random 256-bit tokens kept only as SHA-256 on disk.
Sensitive actions need a fresh assertion (step-up, 5 minutes). Weak sessions can never enroll or revoke devices
unless the request is physically local.
"""
import hashlib
import os
import ipaddress
import secrets
import threading
import time

from . import webauthn
from .store import SESSION_TTL
from .webauthn import WebAuthnError, b64u

CHALLENGE_TTL = 120
FRESH_TTL = int(os.environ.get("PLAYBOX_HUB_FRESH", "300"))   # env override exists so tests can expire step-up quickly
PAIR_TTL = 600
PROXY_HEADERS = ("x-forwarded-for", "forwarded", "cf-connecting-ip", "x-real-ip", "tailscale-user-login", "cf-ray")

def hostname_of(host):
    host = host or ""
    return host[1:host.find("]")] if host.startswith("[") else host.split(":")[0]


sha = lambda s: hashlib.sha256(s.encode()).hexdigest()


class AuthError(Exception):
    def __init__(self, code, msg):
        super().__init__(msg)
        self.code, self.msg = code, msg


class Auth:
    def __init__(self, store, allow_weak=True):
        self.s = store
        self.allow_weak = allow_weak
        self.tickets = {}          # ticket -> {challenge, kind, rp, exp, ...}
        self.fails = {}            # ip -> [timestamps]
        self.mu = threading.Lock()

    # ---- pairing codes ----
    def mint_code(self, ttl=PAIR_TTL, note=""):
        code = secrets.token_urlsafe(18)
        with self.s.lock:
            self.s.d["pair"][sha(code)] = {"exp": time.time() + ttl, "note": note}
            self.s.save()
        return code

    def _take_code(self, code):
        with self.s.lock:
            ent = self.s.d["pair"].get(sha(code or ""))
            if not ent or ent["exp"] < time.time():
                return None
            return ent

    def _burn_code(self, code):
        with self.s.lock:
            self.s.d["pair"].pop(sha(code), None)

    # ---- throttling ----
    def throttle(self, ip):
        now = time.time()
        with self.mu:
            q = [t for t in self.fails.get(ip, []) if now - t < 300]
            self.fails[ip] = q
            if len(q) >= 8:
                raise AuthError(429, "too many attempts; wait a few minutes")

    def _fail(self, ip):
        with self.mu:
            self.fails.setdefault(ip, []).append(time.time())

    # ---- tickets ----
    def _ticket(self, **kw):
        t = secrets.token_urlsafe(16)
        with self.mu:
            now = time.time()
            self.tickets = {k: v for k, v in self.tickets.items() if v["exp"] > now}
            self.tickets[t] = {"exp": now + CHALLENGE_TTL, **kw}
        return t

    def _use_ticket(self, t, kind):
        with self.mu:
            ent = self.tickets.pop(t, None)
        if not ent or ent["exp"] < time.time() or ent["kind"] != kind:
            raise AuthError(400, "challenge expired; try again")
        return ent

    # ---- sessions ----
    def new_session(self, cred_id, weak, ua, ip, name):
        tok = secrets.token_urlsafe(32)
        now = time.time()
        with self.s.lock:
            self.s.gc(now)
            self.s.d["sessions"][sha(tok)] = {"cred": cred_id, "weak": weak, "created": now, "last": now, "ua": (ua or "")[:90],
                                              "ip": ip, "fresh_until": now + FRESH_TTL if not weak else 0, "name": name}
            self.s.save()
        return tok

    def session(self, token):
        """Returns (key, record) or None. Slides the expiry, writing to disk at most once a minute."""
        if not token:
            return None
        key = sha(token)
        now = time.time()
        with self.s.lock:
            rec = self.s.d["sessions"].get(key)
            if not rec or rec["last"] + SESSION_TTL < now:
                return None
            if now - rec["last"] > 60:
                rec["last"] = now
                self.s.save()
            return key, rec

    def logout(self, token):
        with self.s.lock:
            self.s.d["sessions"].pop(sha(token or ""), None)
            self.s.save()

    def is_fresh(self, rec, local):
        if rec["weak"]:
            return local          # a weak session is only ever as strong as physical access to this machine
        return rec["fresh_until"] > time.time()

    # ---- weak path ----
    @staticmethod
    def weak_allowed(peer_ip, host, headers):
        """Plain-HTTP session for a phone on the home network. Refused behind any proxy or tunnel, because
        from the hub's side those look like loopback."""
        if any(h in headers for h in PROXY_HEADERS):
            return False
        h = hostname_of(host)
        host_ok = h in ("localhost",) or h.endswith(".local")
        try:
            host_ok = host_ok or ipaddress.ip_address(h).is_private or ipaddress.ip_address(h).is_loopback
        except ValueError:
            pass
        try:
            ip = ipaddress.ip_address(peer_ip)
            peer_ok = ip.is_private or ip.is_loopback or ip.is_link_local or ip in ipaddress.ip_network("100.64.0.0/10")
        except ValueError:
            peer_ok = False
        return host_ok and peer_ok

    def weak_session(self, code, ua, ip, name):
        if not self.allow_weak:
            raise AuthError(403, "plain-HTTP sessions are disabled on this hub")
        if not self._take_code(code):
            self._fail(ip)
            raise AuthError(403, "pairing code is invalid or expired")
        self._burn_code(code)
        return self.new_session(None, True, ua, ip, name or "Local device")

    # ---- passkey registration ----
    def register_begin(self, code, rp_id, ip):
        if not self._take_code(code):
            self._fail(ip)
            raise AuthError(403, "pairing code is invalid or expired")
        challenge = secrets.token_bytes(32)
        t = self._ticket(kind="reg", challenge=challenge, rp=rp_id, code=code)
        with self.s.lock:
            exclude = [{"type": "public-key", "id": c["id"]} for c in self.s.d["creds"] if c["rp_id"] == rp_id]
            uid = self.s.d["user_id"]
        return {"ticket": t, "publicKey": {
            "rp": {"id": rp_id, "name": "Playbox"}, "user": {"id": uid, "name": "playbox", "displayName": "Playbox owner"},
            "challenge": b64u(challenge), "pubKeyCredParams": [{"type": "public-key", "alg": -7}, {"type": "public-key", "alg": -257}],
            "authenticatorSelection": {"residentKey": "preferred", "userVerification": "required"},
            "attestation": "none", "timeout": 120000, "excludeCredentials": exclude, "hints": ["client-device"]}}

    def register_finish(self, ticket, resp, name, ua, ip):
        ent = self._use_ticket(ticket, "reg")
        if not self._take_code(ent["code"]):
            raise AuthError(403, "pairing code is invalid or expired")
        try:
            cred = webauthn.verify_registration(resp, ent["challenge"], ent["rp"])
        except WebAuthnError as e:
            self._fail(ip)
            raise AuthError(400, str(e))
        with self.s.lock:
            if any(c["id"] == cred["id"] for c in self.s.d["creds"]):
                raise AuthError(409, "this passkey is already enrolled")
            now = time.time()
            self.s.d["creds"].append({**cred, "rp_id": ent["rp"], "name": (name or "Device")[:40], "created": now, "last": now})
            self._burn_code(ent["code"])
            self.s.save()
        return self.new_session(cred["id"], False, ua, ip, name or "Device")

    # ---- passkey login / step-up ----
    def login_begin(self, rp_id, kind="login", cred_id=None):
        with self.s.lock:
            creds = [c for c in self.s.d["creds"] if c["rp_id"] == rp_id and (cred_id is None or c["id"] == cred_id)]
        if not creds:
            raise AuthError(404, "no passkey is enrolled for this address; pair this device first")
        challenge = secrets.token_bytes(32)
        t = self._ticket(kind=kind, challenge=challenge, rp=rp_id)
        return {"ticket": t, "publicKey": {"challenge": b64u(challenge), "rpId": rp_id, "timeout": 120000, "userVerification": "required",
                                           "allowCredentials": [{"type": "public-key", "id": c["id"]} for c in creds]}}

    def assert_finish(self, ticket, cred_id, resp, kind, ip):
        ent = self._use_ticket(ticket, kind)
        with self.s.lock:
            cred = next((c for c in self.s.d["creds"] if c["id"] == cred_id and c["rp_id"] == ent["rp"]), None)
        if not cred:
            self._fail(ip)
            raise AuthError(403, "unknown passkey")
        try:
            count = webauthn.verify_assertion(resp, ent["challenge"], ent["rp"], cred)
        except WebAuthnError as e:
            self._fail(ip)
            raise AuthError(403, str(e))
        with self.s.lock:
            cred["count"], cred["last"] = count, time.time()
            self.s.save()
        return cred

    def stepup(self, session_key, ticket, cred_id, resp, ip):
        with self.s.lock:
            rec = self.s.d["sessions"].get(session_key)
        if not rec or rec["weak"] or rec["cred"] != cred_id:
            raise AuthError(403, "step-up must use this session's own passkey")
        self.assert_finish(ticket, cred_id, resp, "stepup", ip)
        with self.s.lock:
            rec["fresh_until"] = time.time() + FRESH_TTL
            self.s.save()

    # ---- device management ----
    def devices(self, current_cred):
        with self.s.lock:
            return [{"id": c["id"], "name": c["name"], "created": c["created"], "last": c["last"], "rp_id": c["rp_id"], "current": c["id"] == current_cred}
                    for c in self.s.d["creds"]]

    def revoke_device(self, cred_id):
        with self.s.lock:
            n = len(self.s.d["creds"])
            self.s.d["creds"] = [c for c in self.s.d["creds"] if c["id"] != cred_id]
            self.s.d["sessions"] = {k: v for k, v in self.s.d["sessions"].items() if v["cred"] != cred_id}
            self.s.save()
            return len(self.s.d["creds"]) < n

    def sessions(self, current_key):
        with self.s.lock:
            return [{"key": k[:16], "name": v["name"], "weak": v["weak"], "last": v["last"], "created": v["created"], "ip": v["ip"],
                     "current": k == current_key} for k, v in sorted(self.s.d["sessions"].items(), key=lambda kv: -kv[1]["last"])]

    def revoke_session(self, prefix):
        with self.s.lock:
            hit = [k for k in self.s.d["sessions"] if k.startswith(prefix) and len(prefix) >= 8]
            for k in hit:
                del self.s.d["sessions"][k]
            self.s.save()
            return bool(hit)

    def enrolled(self):
        with self.s.lock:
            return bool(self.s.d["creds"])
