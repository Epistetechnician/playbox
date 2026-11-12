#!/usr/bin/env python3
"""Hub tests: P-256 vs OpenSSL, WebAuthn with a software authenticator, then HTTP against a real hub process.

    python3 ui/tests/test_hub.py        # needs playbox-mcp and openssl
"""
import hashlib
import http.client
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
UI = os.path.dirname(HERE)
sys.path.insert(0, UI)
from hub import p256, push, webauthn  # noqa: E402
from hub.auth import Auth, AuthError  # noqa: E402
from hub.store import Store  # noqa: E402
from hub.webauthn import b64u  # noqa: E402

fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + ("" if cond else f"  {detail}"))
    if not cond:
        fails.append(name)


def cbor(v):
    def head(m, n):
        if n < 24:
            return bytes([m << 5 | n])
        for i, ln in enumerate((1, 2, 4, 8)):
            if n < 1 << (8 * ln):
                return bytes([m << 5 | 24 + i]) + n.to_bytes(ln, "big")
    if isinstance(v, int):
        return head(0, v) if v >= 0 else head(1, -1 - v)
    if isinstance(v, bytes):
        return head(2, len(v)) + v
    if isinstance(v, str):
        return head(3, len(v.encode())) + v.encode()
    if isinstance(v, dict):
        return head(5, len(v)) + b"".join(cbor(k) + cbor(x) for k, x in v.items())
    raise TypeError(v)


class Authenticator:
    """A software passkey: what a phone's secure enclave does, minus the enclave."""
    def __init__(self):
        self.d, self.pub = p256.generate()
        self.cid = os.urandom(32)
        self.count = 0

    def create(self, opts, origin, uv=True, challenge=None):
        rp = opts["rp"]["id"]
        cd = json.dumps({"type": "webauthn.create", "challenge": challenge or opts["challenge"], "origin": origin}).encode()
        cose = cbor({1: 2, 3: -7, -1: 1, -2: self.pub[0].to_bytes(32, "big"), -3: self.pub[1].to_bytes(32, "big")})
        ad = hashlib.sha256(rp.encode()).digest() + bytes([0x45 if uv else 0x41]) + (0).to_bytes(4, "big") + bytes(16) + len(self.cid).to_bytes(2, "big") + self.cid + cose
        return {"clientDataJSON": b64u(cd), "attestationObject": b64u(cbor({"fmt": "none", "attStmt": {}, "authData": ad}))}

    def get(self, opts, origin, uv=True, bump=True, rp=None, kind="webauthn.get"):
        rp = rp or opts["rpId"]
        if bump:
            self.count += 1
        cd = json.dumps({"type": kind, "challenge": opts["challenge"], "origin": origin}).encode()
        ad = hashlib.sha256(rp.encode()).digest() + bytes([0x05 if uv else 0x01]) + self.count.to_bytes(4, "big")
        r, s = p256.sign(self.d, ad + hashlib.sha256(cd).digest())
        return {"clientDataJSON": b64u(cd), "authenticatorData": b64u(ad), "signature": b64u(p256.der_encode(r, s))}


def refuses(fn, *a, **k):
    try:
        fn(*a, **k)
    except (AuthError, webauthn.WebAuthnError, ValueError):
        return True
    return False


def test_crypto():
    tmp = tempfile.mkdtemp()
    sh = lambda *a, **k: subprocess.run(a, capture_output=True, **k)
    sh("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", tmp + "/k.pem")
    der = sh("openssl", "ec", "-in", tmp + "/k.pem", "-pubout", "-outform", "DER").stdout
    open(tmp + "/m", "wb").write(b"playbox")
    sig = sh("openssl", "dgst", "-sha256", "-sign", tmp + "/k.pem", tmp + "/m").stdout
    pub = (int.from_bytes(der[-64:-32], "big"), int.from_bytes(der[-32:], "big"))
    r, s = p256.der_decode(sig)
    check("p256: verifies an OpenSSL signature", p256.verify(pub, b"playbox", r, s))
    check("p256: rejects a different message", not p256.verify(pub, b"playbOx", r, s))
    d, q = p256.generate()
    r, s = p256.sign(d, b"hello")
    open(tmp + "/pub.der", "wb").write(bytes.fromhex("3059301306072a8648ce3d020106082a8648ce3d030107034200") + p256.raw_pub(q))
    open(tmp + "/m2", "wb").write(b"hello")
    open(tmp + "/sig", "wb").write(p256.der_encode(r, s))
    ok = sh("openssl", "dgst", "-sha256", "-verify", tmp + "/pub.der", "-keyform", "DER", "-signature", tmp + "/sig", tmp + "/m2")
    check("p256: OpenSSL verifies our signature", ok.returncode == 0, ok.stderr)
    check("p256: rejects off-curve key", not p256.verify((1, 2), b"x", r, s))
    check("p256: DER round trip", p256.der_decode(p256.der_encode(r, s)) == (r, s))
    check("p256: malformed DER refused", refuses(p256.der_decode, b"\x30\x05\x02"))
    # VAPID token verifies under the public key advertised in the same header
    vd, vq = p256.generate()
    hdr = push.vapid_header(vd, b64u(p256.raw_pub(vq)), "https://push.example.net/send/abc")
    jwt = re.search(r"t=([^,]+)", hdr).group(1)
    h, c, sg = jwt.split(".")
    raw = webauthn.unb64u(sg)
    claims = json.loads(webauthn.unb64u(c))
    check("vapid: ES256 token verifies, aud is the push origin", p256.verify(vq, (h + "." + c).encode(), int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big"))
          and claims["aud"] == "https://push.example.net" and claims["exp"] > time.time())
    shutil.rmtree(tmp)


def test_webauthn():
    home = tempfile.mkdtemp()
    a = Auth(Store(home))
    origin, rp = "https://hub.tail1234.ts.net", "hub.tail1234.ts.net"
    code = a.mint_code()
    dev = Authenticator()
    b = a.register_begin(code, rp, "1.1.1.1")
    resp = dev.create(b["publicKey"], origin)
    check("webauthn: bad pairing code refused", refuses(a.register_begin, "nope", rp, "1.1.1.1"))
    check("webauthn: wrong origin refused", refuses(a.register_finish, b["ticket"], dev.create(b["publicKey"], "https://evil.example"), "x", "", "1.1.1.1"))
    b = a.register_begin(code, rp, "1.1.1.1")
    check("webauthn: missing user verification refused", refuses(a.register_finish, b["ticket"], dev.create(b["publicKey"], origin, uv=False), "x", "", "1.1.1.1"))
    b = a.register_begin(code, rp, "1.1.1.1")
    check("webauthn: wrong challenge refused", refuses(a.register_finish, b["ticket"], dev.create(b["publicKey"], origin, challenge=b64u(os.urandom(32))), "x", "", "1.1.1.1"))
    b = a.register_begin(code, rp, "1.1.1.1")
    tok = a.register_finish(b["ticket"], dev.create(b["publicKey"], origin), "Phone", "ua", "1.1.1.1")
    check("webauthn: enrollment yields a strong session", bool(a.session(tok)) and not a.session(tok)[1]["weak"])
    check("webauthn: pairing code is single use", refuses(a.register_begin, code, rp, "1.1.1.1"))
    lb = a.login_begin(rp)
    check("webauthn: no passkey for another address", refuses(a.login_begin, "other.example"))
    resp = dev.get(lb["publicKey"], origin)
    cred = a.assert_finish(lb["ticket"], b64u(dev.cid), resp, "login", "1.1.1.1")
    check("webauthn: assertion accepted", cred["count"] == 1)
    lb = a.login_begin(rp)
    check("webauthn: stale counter (cloned key) refused", refuses(a.assert_finish, lb["ticket"], b64u(dev.cid), dev.get(lb["publicKey"], origin, bump=False), "login", "1.1.1.1"))
    lb = a.login_begin(rp)
    other = Authenticator(); other.cid = dev.cid
    check("webauthn: signature from another key refused", refuses(a.assert_finish, lb["ticket"], b64u(dev.cid), other.get(lb["publicKey"], origin), "login", "1.1.1.1"))
    lb = a.login_begin(rp)
    check("webauthn: assertion for another rpId refused", refuses(a.assert_finish, lb["ticket"], b64u(dev.cid), dev.get(lb["publicKey"], origin, rp="other.example"), "login", "1.1.1.1"))
    lb = a.login_begin(rp)
    r = dev.get(lb["publicKey"], origin)
    a.assert_finish(lb["ticket"], b64u(dev.cid), r, "login", "1.1.1.1")
    check("webauthn: a ticket cannot be replayed", refuses(a.assert_finish, lb["ticket"], b64u(dev.cid), r, "login", "1.1.1.1"))
    for _ in range(8):
        a._fail("9.9.9.9")
    check("auth: throttle after repeated failures", refuses(a.throttle, "9.9.9.9"))
    check("auth: weak path refused behind a proxy header", not Auth.weak_allowed("127.0.0.1", "hub.example.com", {"x-forwarded-for"}) and
          not Auth.weak_allowed("127.0.0.1", "hub.example.com", set()) and Auth.weak_allowed("192.168.1.5", "192.168.1.2:7717", set()) and
          not Auth.weak_allowed("8.8.8.8", "192.168.1.2:7717", set()))
    shutil.rmtree(home)


class Client:
    def __init__(self, port):
        self.port, self.cookie = port, None

    def req(self, method, path, body=None, headers=None, origin=True):
        h = {"Host": f"127.0.0.1:{self.port}", **(headers or {})}
        if body is not None:
            h["Content-Type"] = "application/json"
        if origin and method != "GET":
            h.setdefault("Origin", f"http://127.0.0.1:{self.port}")
        if self.cookie:
            h["Cookie"] = "pbs=" + self.cookie
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        c.request(method, path, json.dumps(body) if body is not None else None, h)
        r = c.getresponse()
        raw = r.read()
        sc = r.getheader("Set-Cookie")
        if sc and sc.startswith("pbs="):
            self.cookie = sc.split(";")[0][4:] or None
        c.close()
        try:
            return r.status, json.loads(raw), r
        except ValueError:
            return r.status, raw, r


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def test_http():
    mcp = os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp")
    if not mcp:
        print("SKIP http tests (playbox-mcp not found)")
        return
    home, port = tempfile.mkdtemp(prefix="pbhub-"), free_port()
    p = subprocess.Popen([sys.executable, os.path.join(UI, "playbox_hub.py"), "--port", str(port), "--home", home, "--mcp", mcp],
                         stdout=subprocess.PIPE, text=True, env={**os.environ, "PLAYBOX_HUB_FRESH": "2"})
    try:
        code = None
        for _ in range(12):
            m = re.search(r"/pair#(\S+)", p.stdout.readline())
            if m:
                code = m.group(1)
                break
        check("hub prints a one-time pairing link", bool(code))
        a, anon = Client(port), Client(port)
        st, j, r = anon.req("GET", "/api/auth/state")
        check("state: not enrolled, not authed", st == 200 and j["enrolled"] is False and j["authed"] is False)
        check("api locked without a session", anon.req("GET", "/api/rooms")[0] == 401)
        check("room page redirects to lock screen", anon.req("GET", "/r/abc/")[0] == 303)
        h = r.headers
        check("security headers set", "frame-ancestors 'none'" in h["Content-Security-Policy"] and h["X-Frame-Options"] == "DENY" and h["X-Content-Type-Options"] == "nosniff")
        st, j, r = anon.req("GET", "/manifest.webmanifest")
        check("manifest has 192/512/maskable icons", st == 200 and {i["sizes"] + i["purpose"] for i in j["icons"]} >= {"192x192any", "512x512maskable"})
        st, raw, r = anon.req("GET", "/icon-512.png")
        check("icon-512 is a PNG", st == 200 and raw[:8] == b"\x89PNG\r\n\x1a\n")
        st, raw, r = anon.req("GET", "/sw.js")
        check("service worker served with root scope", st == 200 and r.getheader("Service-Worker-Allowed") == "/")
        check("weak pairing refused through a proxy", a.req("POST", "/api/auth/weak", {"code": code}, {"X-Forwarded-For": "1.2.3.4"})[0] == 403)
        check("cross-origin POST refused", a.req("POST", "/api/auth/weak", {"code": code}, {"Origin": "https://evil.example"})[0] == 403)
        check("wrong code refused", a.req("POST", "/api/auth/weak", {"code": "nope"})[0] == 403)
        st, j, _ = a.req("POST", "/api/auth/weak", {"code": code, "name": "Test"})
        check("weak pairing on a private network works", st == 200 and a.cookie)
        check("pairing code is single use", Client(port).req("POST", "/api/auth/weak", {"code": code})[0] == 403)

        st, me, _ = a.req("GET", "/api/me")
        check("me: weak session flagged", st == 200 and me["weak"] is True and me["push_key"])
        st, j, _ = a.req("POST", "/api/rooms", {"label": "Lab agents", "machine": "studio-mac", "kind": "mine"})
        rid = j.get("id")
        check("add a room", st == 200 and rid)
        d = None
        for _ in range(40):
            d = next((x for x in a.req("GET", "/api/rooms")[1]["rooms"] if x["id"] == rid), None)
            if d and d["state"] != "down":
                break
            time.sleep(0.25)
        check("poller reports the room (empty, mine, machine label)", d and d["state"] == "empty" and d["kind"] == "mine" and d["machine"] == "studio-mac", str(d))
        st, j, _ = a.req("GET", f"/r/{rid}/api/state")
        check("room proxy: sidecar status", st == 200 and j["ok"] and "playbox_status" in j["tools"])
        st, raw, _ = a.req("GET", f"/r/{rid}/")
        check("room proxy: cockpit served with base-path support", st == 200 and b"const BASE" in raw)
        check("room proxy: unknown tool refused", a.req("POST", f"/r/{rid}/api/tool", {"name": "rm_rf", "args": {}})[0] == 400)
        check("room proxy: unknown room 404", a.req("GET", "/r/zzzz/api/state")[0] == 404)
        st, j, _ = a.req("PATCH", f"/api/rooms/{rid}", {"label": "Lab", "kind": "shared"})
        check("rename + reclassify", st == 200 and j["room"]["label"] == "Lab" and j["room"]["kind"] == "shared")
        st, j, _ = a.req("POST", "/api/rooms", {"label": "x", "home": "/nonexistent/zz"})
        check("adopting a missing home refused", st == 400)

        c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        c.request("GET", "/api/events", headers={"Host": f"127.0.0.1:{port}", "Cookie": "pbs=" + a.cookie})
        r = c.getresponse()
        buf = b""
        while b"event: hello" not in buf or b"\n\n" not in buf.split(b"event: hello")[1]:
            buf += r.read1(4096) if hasattr(r, "read1") else r.read(1)
        c.close()
        check("SSE: hello carries the rooms", r.status == 200 and rid.encode() in buf)

        # a weak session behind a proxy cannot manage devices; locally it can
        check("weak session behind proxy cannot pair devices", a.req("POST", "/api/devices/pair", {}, {"X-Forwarded-For": "1.1.1.1"})[0] == 403)
        st, j, _ = a.req("POST", "/api/devices/pair", {})
        check("weak session on the hub machine itself can", st == 200 and j["code"])

        # enroll a passkey with a software authenticator, as a browser would
        dev, s = Authenticator(), Client(port)
        origin = f"http://127.0.0.1:{port}"
        st, b, _ = s.req("POST", "/api/auth/register/begin", {"code": j["code"]})
        st, f, _ = s.req("POST", "/api/auth/register/finish", {"ticket": b["ticket"], "name": "Soft key", "response": dev.create(b["publicKey"], origin)})
        check("passkey enrollment over HTTP", st == 200 and s.cookie, str(f))
        st, me, _ = s.req("GET", "/api/me")
        check("me: strong session lists both flows", st == 200 and me["weak"] is False and any(x["name"] == "Soft key" and x["current"] for x in me["devices"]))
        s.cookie = None
        check("signed out after cookie loss", s.req("GET", "/api/rooms")[0] == 401)
        st, lb, _ = s.req("POST", "/api/auth/login/begin", {})
        st, f, _ = s.req("POST", "/api/auth/login/finish", {"ticket": lb["ticket"], "id": b64u(dev.cid), "response": dev.get(lb["publicKey"], origin)})
        check("passkey login", st == 200 and s.cookie, str(f))

        time.sleep(2.3)   # let the step-up window lapse (PLAYBOX_HUB_FRESH=2)
        st, j, _ = s.req("POST", f"/r/{rid}/api/tool", {"name": "playbox_revoke", "args": {"member_id": "x"}})
        check("sensitive tool needs step-up", st == 403 and j["error"] == "stepup")
        check("non-sensitive tool does not", s.req("POST", f"/r/{rid}/api/tool", {"name": "playbox_status", "args": {}})[0] == 200)
        st, sb, _ = s.req("POST", "/api/auth/stepup/begin", {})
        st, _, _ = s.req("POST", "/api/auth/stepup/finish", {"ticket": sb["ticket"], "id": b64u(dev.cid), "response": dev.get(sb["publicKey"], origin, kind="webauthn.get")})
        check("step-up ceremony accepted", st == 200)
        check("sensitive tool allowed after step-up", s.req("POST", f"/r/{rid}/api/tool", {"name": "playbox_revoke", "args": {"member_id": "x"}})[0] == 200)
        st, sb, _ = s.req("POST", "/api/auth/stepup/begin", {})
        other = Authenticator(); other.cid = dev.cid
        check("step-up with a different key refused", s.req("POST", "/api/auth/stepup/finish", {"ticket": sb["ticket"], "id": b64u(dev.cid), "response": other.get(sb["publicKey"], origin)})[0] == 403)

        st, sess, _ = s.req("GET", "/api/me")
        check("device removal by the weak session is blocked behind a proxy", a.req("DELETE", "/api/devices/" + b64u(dev.cid), None, {"X-Forwarded-For": "1.1.1.1"})[0] == 403)
        st, j, _ = s.req("DELETE", "/api/devices/" + b64u(dev.cid))
        check("removing a device ends its sessions", st == 200 and s.req("GET", "/api/rooms")[0] == 401)
        check("room removal works (step-up satisfied locally)", a.req("DELETE", f"/api/rooms/{rid}")[0] == 200 and not any(x["id"] == rid for x in a.req("GET", "/api/rooms")[1]["rooms"]))
        a.req("POST", "/api/auth/logout", {})
        check("logout ends the session", a.req("GET", "/api/rooms")[0] == 401)
        state = open(os.path.join(home, "hub.json")).read()
        check("state file is 0600 and stores no session token or pairing code", oct(os.stat(os.path.join(home, "hub.json")).st_mode & 0o777) == "0o600" and code not in state)
        t = Client(port)
        codes = [t.req("POST", "/api/auth/weak", {"code": "bad"})[0] for _ in range(10)]
        check("throttle kicks in after repeated bad codes", 429 in codes)
    finally:
        p.terminate()
        shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    test_crypto()
    test_webauthn()
    test_http()
    print(f"\n{len(fails)} failed" if fails else "\nall passed")
    sys.exit(1 if fails else 0)
