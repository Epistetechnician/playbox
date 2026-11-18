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
import hmac
import ipaddress
import json
import os
import re
import secrets
import stat
import threading
import time

from . import webauthn
from .store import SESSION_TTL
from .webauthn import WebAuthnError, b64u

CHALLENGE_TTL = 120
FRESH_TTL = int(os.environ.get("PLAYBOX_HUB_FRESH", "300"))   # env override exists so tests can expire step-up quickly
PAIR_TTL = 600
MAX_FAIL_IPS = 4096
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

    def _consume_code(self, code):
        """Check and burn in one step under the store lock: two requests racing with one code cannot both win."""
        with self.s.lock:
            ent = self.s.d["pair"].pop(sha(code or ""), None)
            return bool(ent and ent["exp"] >= time.time())

    def _burn_code(self, code):
        with self.s.lock:
            self.s.d["pair"].pop(sha(code), None)

    # ---- throttling ----
    def throttle(self, ip):
        now = time.time()
        with self.mu:
            q = [t for t in self.fails.get(ip, []) if now - t < 300]
            if q:
                self.fails[ip] = q
            else:
                self.fails.pop(ip, None)
            if len(q) >= 8:
                raise AuthError(429, "too many attempts; wait a few minutes")

    def _fail(self, ip):
        now = time.time()
        with self.mu:
            q = [t for t in self.fails.pop(ip, []) if now - t < 300]
            q.append(now)
            self.fails[ip] = q
            if len(self.fails) > MAX_FAIL_IPS:
                stale = [k for k, v in self.fails.items() if not v or now - v[-1] >= 300]
                for k in stale:
                    del self.fails[k]
                while len(self.fails) > MAX_FAIL_IPS:
                    del self.fails[next(iter(self.fails))]

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
    def new_session(self, cred_id, weak, ua, ip, name, local_only=False):
        tok = secrets.token_urlsafe(32)
        now = time.time()
        with self.s.lock:
            self.s.gc(now)
            self.s.d["sessions"][sha(tok)] = {"cred": cred_id, "weak": weak, "created": now, "last": now, "ua": (ua or "")[:90],
                                              "ip": ip, "fresh_until": now + FRESH_TTL if not weak else 0, "name": name, "local_only": local_only}
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
        if rec.get("local_only"):
            return local and rec.get("fresh_until", 0) > time.time()
        if rec["weak"]:
            return local          # a weak pairing-code session on loopback keeps existing behaviour
        return rec["fresh_until"] > time.time()

    # ---- weak path ----
    @staticmethod
    def weak_allowed(peer_ip, host, headers):
        """Plain-HTTP session for a phone on the home network. Refused behind any proxy or tunnel, because
        from the hub's side those look like loopback."""
        if any(h in headers for h in PROXY_HEADERS):
            return False
        h = hostname_of(host).lower()
        host_ok = h in ("localhost",) or h.endswith((".local", ".ts.net"))
        try:
            ha = ipaddress.ip_address(h)
            host_ok = host_ok or ha.is_private or ha.is_loopback or ha in ipaddress.ip_network("100.64.0.0/10")
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
        if not self._consume_code(code):
            self._fail(ip)
            raise AuthError(403, "pairing code is invalid or expired")
        return self.new_session(None, True, ua, ip, name or "Local device")

    # ---- desktop sign-in & owner step-up ----
    LOGIN_DIR = "desktop-login"
    LOGIN_TTL = 60
    STEPUP_DIR = "desktop-stepup"

    def local_session(self, nonce, ua, ip):
        """The desktop app proves it can write into this hub's private state directory (owner-only, so it is the same user on this
        machine, the same root of trust as `--pair`) by creating `desktop-login/<sha256(nonce)>`. One use, 60 seconds, owner-only file.
        The session it opens is weak (never as strong as a passkey) and local-only: it is ignored on any request that is not loopback."""
        if not re.fullmatch(r"[A-Za-z0-9_-]{32,64}", nonce or ""):
            self._fail(ip)
            raise AuthError(403, "desktop sign-in refused")
        d = self.s.home / self.LOGIN_DIR
        f = d / sha(nonce)
        ok = False
        try:
            me = os.getuid() if hasattr(os, "getuid") else None
            st, dst = f.lstat(), d.lstat()
            ok = (stat.S_ISREG(st.st_mode) and stat.S_ISDIR(dst.st_mode) and time.time() - st.st_mtime <= self.LOGIN_TTL
                  and (me is None or (st.st_uid == me and dst.st_uid == me and not st.st_mode & 0o077 and not dst.st_mode & 0o022)))
        except OSError:
            pass
        try:
            f.unlink()          # single use, whether or not it was good
        except OSError:
            pass
        if not ok:
            self._fail(ip)
            raise AuthError(403, "desktop sign-in refused")
        return self.new_session(None, True, ua, ip, "Playbox Desktop", local_only=True)

    def desktop_bridge_ready(self):
        d = self.s.home / self.STEPUP_DIR
        f = d / "ready"
        try:
            me = os.getuid() if hasattr(os, "getuid") else None
            st, dst = f.lstat(), d.lstat()
            return bool(stat.S_ISREG(st.st_mode) and stat.S_ISDIR(dst.st_mode) and time.time() - st.st_mtime <= 35
                        and not (st.st_mode & 0o077) and not (dst.st_mode & 0o022)
                        and (me is None or (st.st_uid == me and dst.st_uid == me)))
        except OSError:
            return False

    def desktop_stepup(self, session_key, reason, ip):
        with self.s.lock:
            rec = self.s.d["sessions"].get(session_key)
        if not rec or not rec.get("local_only"):
            raise AuthError(403, "desktop owner verification is only for Playbox Desktop sessions")
        if not self.desktop_bridge_ready():
            raise AuthError(403, "Playbox Desktop owner verification is not running; approve this in your browser with a passkey")
        req_id = secrets.token_hex(16)
        nonce = secrets.token_urlsafe(32)
        clean_reason = re.sub(r"[\x00-\x1f\x7f]", " ", str(reason or "Approve sensitive action in Playbox")).strip()[:120] or "Approve sensitive action in Playbox"
        d = self.s.home / self.STEPUP_DIR
        d.mkdir(mode=0o700, parents=True, exist_ok=True)
        tmp_path = d / f"{req_id}.req.tmp"
        req_path = d / f"{req_id}.req"
        res_path = d / f"{req_id}.res"
        payload = json.dumps({"id": req_id, "nonce": nonce, "reason": clean_reason, "exp": int(time.time()) + 60}).encode()
        try:
            fd = os.open(tmp_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            try:
                os.write(fd, payload)
            finally:
                os.close(fd)
            os.replace(tmp_path, req_path)
            wait_s = max(0.2, min(120.0, float(os.environ.get("PLAYBOX_STEPUP_WAIT_S", "55"))))
            deadline = time.monotonic() + wait_s
            me = os.getuid() if hasattr(os, "getuid") else None
            while time.monotonic() < deadline:
                try:
                    st = res_path.lstat()
                except FileNotFoundError:
                    time.sleep(0.08)
                    continue
                except OSError:
                    raise AuthError(403, "Touch ID / owner verification failed")
                dst = d.lstat()
                mode_ok = (stat.S_ISREG(st.st_mode) and stat.S_ISDIR(dst.st_mode)
                           and not (st.st_mode & 0o077) and not (dst.st_mode & 0o022)
                           and (me is None or (st.st_uid == me and dst.st_uid == me)))
                raw = b""
                if mode_ok:
                    try:
                        raw = res_path.read_bytes()
                        if not raw:
                            time.sleep(0.02)
                            raw = res_path.read_bytes()
                    except OSError:
                        mode_ok = False
                try:
                    res_path.unlink()
                except OSError:
                    pass
                if not mode_ok:
                    self._fail(ip)
                    raise AuthError(403, "Touch ID / owner verification response was invalid")
                try:
                    data = json.loads(raw.decode("utf-8"))
                except (UnicodeDecodeError, ValueError):
                    self._fail(ip)
                    raise AuthError(403, "Touch ID / owner verification response was invalid")
                if not isinstance(data, dict):
                    self._fail(ip)
                    raise AuthError(403, "Touch ID / owner verification response was invalid")
                expected = hashlib.sha256(("playbox-stepup-v1:" + nonce).encode()).hexdigest()
                if data.get("ok") is True and isinstance(data.get("proof"), str) and hmac.compare_digest(data["proof"], expected):
                    with self.s.lock:
                        rec2 = self.s.d["sessions"].get(session_key)
                        if not rec2 or not rec2.get("local_only"):
                            raise AuthError(401, "session ended")
                        rec2["fresh_until"] = time.time() + FRESH_TTL
                        self.s.save()
                    return True
                if data.get("ok") is True:
                    self._fail(ip)
                raise AuthError(403, str(data.get("error") or "Touch ID / owner verification was cancelled")[:120])
            raise AuthError(408, "Timed out waiting for Touch ID / owner verification")
        finally:
            for p in (tmp_path, req_path, res_path):
                try:
                    p.unlink()
                except OSError:
                    pass

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
            if not self._take_code(ent["code"]):       # re-checked under the lock that burns it: one code enrolls one passkey, however many tickets raced
                raise AuthError(403, "pairing code is invalid or expired")
            if any(c["id"] == cred["id"] for c in self.s.d["creds"]):
                raise AuthError(409, "this passkey is already enrolled")
            now = time.time()
            self.s.d["creds"].append({**cred, "rp_id": ent["rp"], "name": (name or "Device")[:40], "created": now, "last": now})
            self._burn_code(ent["code"])
            self.s.save()
        return self.new_session(cred["id"], False, ua, ip, name or "Device")

    def upgrade_begin(self, session_key: str, rp_id: str, ip: str):
        with self.s.lock:
            if session_key not in self.s.d["sessions"]:
                raise AuthError(401, "session ended")
        code = self.mint_code(note="session upgrade")
        return self.register_begin(code, rp_id, ip)

    def upgrade_finish(self, session_key: str, ticket: str, resp: dict, name: str, ua: str, ip: str) -> str:
        with self.s.lock:
            if session_key not in self.s.d["sessions"]:
                raise AuthError(401, "session ended")
        tok = self.register_finish(ticket, resp, name, ua, ip)
        new_key = sha(tok)
        with self.s.lock:
            if session_key != new_key and session_key in self.s.d["sessions"]:
                del self.s.d["sessions"][session_key]
                self.s.save()
        return tok

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
            return [{"key": k[:16], "name": v["name"], "weak": v["weak"], "local_only": bool(v.get("local_only")), "last": v["last"], "created": v["created"], "ip": v["ip"],
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

    def has_passkey(self, rp_id: str) -> bool:
        with self.s.lock:
            return any(c.get("rp_id") == rp_id for c in self.s.d["creds"])
