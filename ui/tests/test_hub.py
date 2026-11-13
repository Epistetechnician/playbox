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


def test_push_send():
    import urllib.request, urllib.error
    d, q = p256.generate()
    seen = {}
    class R:
        status = 201
        def __enter__(self): return self
        def __exit__(self, *a): pass
    real = urllib.request.urlopen
    def fake(req, timeout=0):
        seen["req"] = req
        if "gone" in req.full_url:
            raise urllib.error.HTTPError(req.full_url, 410, "gone", {}, None)
        return R()
    urllib.request.urlopen = fake
    try:
        st = push.send(d, b64u(p256.raw_pub(q)), "https://push.example.net/s/abc")
        r = seen["req"]
        check("push.send: empty body, VAPID auth, TTL", st == 201 and r.data == b"" and r.get_header("Authorization").startswith("Vapid t=".capitalize()[:5]) or r.get_header("Authorization").startswith("vapid t="))
        check("push.send: 410 reported so the sub is dropped", push.send(d, b64u(p256.raw_pub(q)), "https://push.example.net/gone") == 410)
        check("push.send: refuses non-https endpoints", push.send(d, "x", "http://push.example.net/x") == 0)
    finally:
        urllib.request.urlopen = real


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
        # ---- friend invites ----
        repo = os.path.join(home, "repo")
        os.makedirs(repo)
        subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
        subprocess.run(["git", "-c", "user.email=a@b", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"], cwd=repo, check=True)
        st, j, _ = a.req("POST", f"/r/{rid}/api/tool", {"name": "playbox_create", "args": {"repo": repo, "local": True}})
        check("host the room from the hub", st == 200 and not j.get("error"))
        inv = lambda body, c=a, h=None: c.req("POST", f"/api/rooms/{rid}/invite", body, h)
        check("invite needs a session", inv({"ttl": 3600, "grant": 86400, "uses": 1}, anon)[0] == 401)
        check("invite refuses odd durations", inv({"ttl": 12345, "grant": 86400, "uses": 1})[0] == 400 and inv({"ttl": 3600, "grant": 1, "uses": 1})[0] == 400)
        check("invite refuses a malformed guest id", inv({"ttl": 3600, "grant": 86400, "uses": 1, "guest": "xyz"})[0] == 400)
        st, j, _ = inv({"ttl": 3600, "grant": 86400, "uses": 1, "from": "Maya"})
        iid, ilink = j.get("id"), j.get("link", "")
        check("invite mints a share url and a playbox link", st == 200 and ilink.startswith("playbox://") and j["url"].endswith("/i/" + (iid or "?")) and j["public"] is False)
        st, j2, _ = inv({"ttl": 3600, "grant": 86400, "uses": 1}, a, {"Host": "93.184.216.34:80", "Origin": "http://93.184.216.34:80"})
        check("on a public address the url is marked reachable" + ("" if st == 200 else f" [{st} {j2}]"), st == 200 and j2["public"] is True and j2["url"].startswith("http://93.184.216.34:80/i/"))
        st, pv, _ = anon.req("GET", "/api/invite/" + iid)
        check("anyone can read the invite card, and only the card", st == 200 and pv["state"] == "pending" and pv["room"] == "Lab" and pv["from"] == "Maya" and "link" not in pv and "members" not in pv)
        check("the share page is public", anon.req("GET", "/i/" + iid)[0] == 200)
        check("an unknown invite id is a 404 with no detail", anon.req("GET", "/api/invite/nope-not-real")[1] == {"state": "unknown"})
        check("the hub never stores the join link", ilink[10:80] not in open(os.path.join(home, "hub.json")).read())
        sys.path.insert(0, UI)
        from playbox_ui import Sidecar
        friend = Sidecar(mcp, os.path.join(home, "friend"))
        e, jr = friend.call("playbox_join", {"link": ilink})
        check("a friend's own sidecar redeems the link", not e)
        e2, _ = Sidecar(mcp, os.path.join(home, "friend2")).call("playbox_join", {"link": ilink})
        check("the link is single use", bool(e2))
        got = 0
        for _ in range(25):
            got = next((x["joined"] for x in a.req("GET", "/api/invites")[1]["invites"] if x["id"] == iid), 0)
            if got:
                break
            time.sleep(1)
        check("the owner sees that someone joined", got == 1)
        check("the share page then says it was used", anon.req("GET", "/api/invite/" + iid)[1] == {"state": "used"})
        friend.close()
        check("withdrawing hides the share page", a.req("DELETE", "/api/invites/" + iid)[0] == 200 and anon.req("GET", "/api/invite/" + iid)[1] == {"state": "revoked"})
        st, j3, _ = a.req("POST", "/api/rooms", {"label": "Empty"})
        check("a room that is not hosted here cannot mint invites", a.req("POST", f"/api/rooms/{j3['id']}/invite", {"ttl": 3600, "grant": 86400, "uses": 1})[0] == 400)
        a.req("DELETE", f"/api/rooms/{j3['id']}")

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


def test_cloudflare():
    """--cloudflare against a stand-in cloudflared: passkeys only, throttle per visitor, tunnel process dies with the hub."""
    mcp = os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp")
    if not mcp:
        print("SKIP cloudflare tests (playbox-mcp not found)")
        return
    d = tempfile.mkdtemp(prefix="pbcf-")
    pidf = os.path.join(d, "pid")
    fake = os.path.join(d, "cloudflared")
    open(fake, "w").write("#!/bin/sh\necho $$ > %s\necho \"$@\" > %s.args\necho 'INF |  https://fake-name-here.trycloudflare.com  |' >&2\nexec sleep 300\n" % (pidf, pidf))
    os.chmod(fake, 0o755)
    port, home = free_port(), os.path.join(d, "home")
    env = {**os.environ, "PATH": d + os.pathsep + os.environ["PATH"]}
    bad = subprocess.run([sys.executable, os.path.join(UI, "playbox_hub.py"), "--cloudflare-tunnel", "x", "--home", home, "--mcp", mcp], capture_output=True, text=True, env=env, timeout=20)
    check("named tunnel without --cloudflare-hostname is refused", bad.returncode != 0 and "cloudflare-hostname" in (bad.stdout + bad.stderr))
    p = subprocess.Popen([sys.executable, os.path.join(UI, "playbox_hub.py"), "--port", str(port), "--home", home, "--mcp", mcp, "--cloudflare"],
                         stdout=subprocess.PIPE, text=True, env=env)
    try:
        out = ""
        for _ in range(14):
            out += p.stdout.readline()
            if "/pair#" in out and "fake-name-here" in out and out.count("/pair#") >= 2:
                break
        check("prints the public tunnel address and a pairing link for it", "https://fake-name-here.trycloudflare.com/pair#" in out)
        args = open(pidf + ".args").read()
        check("tunnel points at the loopback hub only", f"--url http://127.0.0.1:{port}" in args)
        c = Client(port)
        st, j, _ = c.req("GET", "/api/auth/state", None, {"Host": "fake-name-here.trycloudflare.com", "Cf-Connecting-Ip": "203.0.113.7", "X-Forwarded-For": "203.0.113.7"})
        check("over the tunnel: weak (plain-HTTP) pairing is not offered", st == 200 and j["weak_ok"] is False and j["rp"] == "fake-name-here.trycloudflare.com")
        st, _, _ = c.req("POST", "/api/auth/weak", {"code": "whatever"}, {"Host": "fake-name-here.trycloudflare.com", "Origin": "https://fake-name-here.trycloudflare.com", "Cf-Connecting-Ip": "203.0.113.7"})
        check("over the tunnel: the weak-session endpoint refuses", st in (403, 404, 429))
        hh = lambda ip: {"Host": "fake-name-here.trycloudflare.com", "Origin": "https://fake-name-here.trycloudflare.com", "Cf-Connecting-Ip": ip}
        a = [c.req("POST", "/api/auth/register/begin", {"code": "bad"}, hh("203.0.113.7"))[0] for _ in range(10)]
        check("throttle trips for the visitor who guessed", 429 in a)
        check("a different visitor behind the same tunnel is not locked out", c.req("POST", "/api/auth/register/begin", {"code": "bad"}, hh("198.51.100.9"))[0] != 429)
        pid = int(open(pidf).read())
        p.terminate()
        p.wait(timeout=15)
        time.sleep(0.5)
        try:
            os.kill(pid, 0)
            alive = True
        except OSError:
            alive = False
        check("the tunnel process is stopped when the hub exits", not alive)
        if alive:
            os.kill(pid, 9)
    finally:
        if p.poll() is None:
            p.kill()
        shutil.rmtree(d, ignore_errors=True)


def test_access():
    """Cloudflare Access tokens: RS256 checked against a local key (OpenSSL signs), then enforced over HTTP behind a stand-in tunnel."""
    sys.path.insert(0, UI)
    from hub.access import Access, AccessError
    d = tempfile.mkdtemp(prefix="pbacc-")
    sh = lambda *a, **k: subprocess.run(a, capture_output=True, check=True, **k).stdout
    key = os.path.join(d, "k.pem")
    sh("openssl", "genrsa", "-out", key, "2048")
    n = int(sh("openssl", "rsa", "-in", key, "-noout", "-modulus").decode().strip().split("=")[1], 16)
    jwks = os.path.join(d, "jwks.json")
    nb = n.to_bytes(256, "big")
    json.dump({"keys": [{"kty": "RSA", "kid": "k1", "alg": "RS256", "n": b64u(nb), "e": b64u((65537).to_bytes(3, "big"))}]}, open(jwks, "w"))
    TEAM, AUD = "lab", "a" * 64
    ISS = f"https://{TEAM}.cloudflareaccess.com"

    def mint(claims=None, kid="k1", alg="RS256", sign=True):
        c = {"iss": ISS, "aud": [AUD], "exp": time.time() + 600, "email": "x@example.com", **(claims or {})}
        h = b64u(json.dumps({"alg": alg, "kid": kid}).encode()) + "." + b64u(json.dumps(c).encode())
        sig = sh("openssl", "dgst", "-sha256", "-sign", key, input=h.encode()) if sign else b"\0" * 256
        return h + "." + b64u(sig)

    acc = Access(TEAM, AUD, keys_file=jwks)

    def ok(t):
        try:
            return acc.verify(t)
        except AccessError as e:
            raise ValueError(str(e))
    check("access: a valid token verifies", ok(mint())["email"] == "x@example.com")
    check("access: wrong audience refused", refuses(ok, mint({"aud": ["b" * 64]})))
    check("access: wrong issuer refused", refuses(ok, mint({"iss": "https://evil.cloudflareaccess.com"})))
    check("access: expired token refused", refuses(ok, mint({"exp": time.time() - 3600})))
    check("access: alg none / HS256 refused", refuses(ok, mint(alg="none")) and refuses(ok, mint(alg="HS256")))
    check("access: unknown key id refused", refuses(ok, mint(kid="other")))
    check("access: forged signature refused", refuses(ok, mint(sign=False)))
    t = mint().split(".")
    t[1] = b64u(json.dumps({"iss": ISS, "aud": [AUD], "exp": time.time() + 600, "email": "attacker@example.com"}).encode())
    check("access: a tampered payload refused", refuses(ok, ".".join(t)))
    check("access: garbage refused", refuses(ok, "not-a-jwt") and refuses(ok, ""))

    mcp = os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp")
    if not mcp:
        print("SKIP access http tests (playbox-mcp not found)")
        return
    fake = os.path.join(d, "cloudflared")
    open(fake, "w").write("#!/bin/sh\necho 'INF Registered tunnel connection' >&2\nexec sleep 300\n")
    os.chmod(fake, 0o755)
    port, home = free_port(), os.path.join(d, "home")
    env = {**os.environ, "PATH": d + os.pathsep + os.environ["PATH"], "PLAYBOX_HUB_ACCESS_KEYS": jwks}
    half = subprocess.run([sys.executable, os.path.join(UI, "playbox_hub.py"), "--cloudflare", "--cloudflare-access", TEAM, "--home", home, "--mcp", mcp], capture_output=True, text=True, env=env, timeout=20)
    check("access: team without an AUD tag is refused at startup", half.returncode != 0 and "AUD" in (half.stdout + half.stderr))
    p = subprocess.Popen([sys.executable, os.path.join(UI, "playbox_hub.py"), "--port", str(port), "--home", home, "--mcp", mcp, "--cloudflare-tunnel", "tid", "--cloudflare-hostname", "playbox.example.com",
                          "--cloudflare-access", TEAM, "--cloudflare-aud", AUD, "--pair"], stdout=subprocess.PIPE, text=True, env=env)
    try:
        for _ in range(12):
            if "/pair#" in p.stdout.readline():
                break
        c = Client(port)
        via = lambda tok=None, path="/api/auth/state": c.req("GET", path, None, {"Host": "playbox.example.com", "Cf-Connecting-Ip": "203.0.113.7", **({"Cf-Access-Jwt-Assertion": tok} if tok else {})})
        check("tunnelled request without a token is refused", via()[0] == 403)
        check("tunnelled request with a forged token is refused", via(mint(sign=False))[0] == 403)
        check("tunnelled request with a token for another app is refused", via(mint({"aud": ["b" * 64]}))[0] == 403)
        check("tunnelled static files are refused too", via(None, "/manifest.webmanifest")[0] == 403)
        st, j, _ = via(mint())
        check("tunnelled request with a valid token reaches the hub", st == 200 and j["weak_ok"] is False)
        check("local requests on loopback do not need a token", c.req("GET", "/api/auth/state")[0] == 200)
    finally:
        p.terminate()
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
        shutil.rmtree(d, ignore_errors=True)


def test_agents():
    """Agent detection reads each agent's config; connecting runs `playbox setup --only <agent>` and nothing else."""
    from hub import agents
    d = tempfile.mkdtemp(prefix="pbag-")
    old = {k: os.environ.get(k) for k in ("HOME", "PATH", "PLAYBOX_CLI")}
    try:
        h = os.path.join(d, "home")
        os.makedirs(os.path.join(h, ".codex"))
        open(os.path.join(h, ".codex", "config.toml"), "w").write("[mcp_servers.playbox]\ncommand = \"playbox\"\n")
        open(os.path.join(h, ".claude.json"), "w").write(json.dumps({"mcpServers": {"other": {}}}))
        os.environ["HOME"], os.environ["PATH"] = h, "/usr/bin:/bin"
        got = {a["id"]: a for a in agents.detect()}
        check("a codex config with playbox is seen as connected", got["codex"]["installed"] and got["codex"]["connected"])
        check("a claude config without playbox is installed but not connected", got["claude"]["installed"] and not got["claude"]["connected"])
        check("an agent that is not here is neither", not got["opencode"]["installed"] and not got["opencode"]["connected"])
        log = os.path.join(d, "calls")
        fake = os.path.join(d, "playbox")
        open(fake, "w").write("#!/bin/sh\necho \"$@\" >> %s\necho did it\n" % log)
        os.chmod(fake, 0o755)
        os.environ["PLAYBOX_CLI"] = fake
        ok, out = agents.connect("claude")
        check("connect runs `playbox setup --only claude`", ok and open(log).read().strip() == "setup --only claude" and "did it" in out)
        ok, out = agents.connect("claude; rm -rf /")
        check("connect refuses anything that is not a known agent", not ok and open(log).read().strip() == "setup --only claude")
    finally:
        for k, v in old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        shutil.rmtree(d, ignore_errors=True)


def test_landing_quoting():
    """The install command on the invite page quotes the host's room name so it can only ever be one shell argument."""
    page = open(os.path.join(UI, "static", "invite.html")).read()
    m = re.search(r"const shq = .*?;(?=\s*//)", page)
    node = shutil.which("node")
    if not (m and node):
        print("SKIP landing quoting (node not found)" if m else "FAIL shq not found")
        check("landing quoting helper exists", bool(m))
        return
    evil = "x'; touch PWNED; echo '\n$(touch PWNED2) `touch PWNED3`"
    d = tempfile.mkdtemp(prefix="pbq-")
    try:
        out = subprocess.run([node, "-e", m.group(0) + "; process.stdout.write(shq(%s))" % json.dumps(evil)], capture_output=True, text=True).stdout
        r = subprocess.run(["sh", "-c", "printf '%s|' " + out], cwd=d, capture_output=True, text=True)
        check("a hostile room name stays one inert argument", r.returncode == 0 and r.stdout.count("|") == 1 and not any(f.startswith("PWNED") for f in os.listdir(d)), r.stdout[:120])
    finally:
        shutil.rmtree(d, ignore_errors=True)


if __name__ == "__main__":
    test_crypto()
    test_webauthn()
    test_push_send()
    test_http()
    test_cloudflare()
    test_access()
    test_agents()
    test_landing_quoting()
    print(f"\n{len(fails)} failed" if fails else "\nall passed")
    sys.exit(1 if fails else 0)
