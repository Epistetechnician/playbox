#!/usr/bin/env python3
"""Hub tests: P-256 vs OpenSSL, WebAuthn with a software authenticator, then HTTP against a real hub process.

    python3 ui/tests/test_hub.py        # needs playbox-mcp and openssl
"""
import hashlib
import http.client
import threading
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


class RsaAuthenticator:
    """RS256 passkey (Windows Hello style), signing with OpenSSL so the pure-Python verifier is cross-checked against it."""
    def __init__(self, bits=2048, e=65537):
        self.dir = tempfile.mkdtemp()
        sh = lambda *a, **k: subprocess.run(a, capture_output=True, check=True, **k)
        sh("openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", f"rsa_keygen_bits:{bits}", "-out", self.dir + "/k.pem")
        txt = sh("openssl", "rsa", "-in", self.dir + "/k.pem", "-noout", "-modulus").stdout.decode()
        self.n = int(txt.strip().split("=")[1], 16)
        self.e = e
        self.cid = os.urandom(32)
        self.count = 0

    def create(self, opts, origin, n=None, e=None, alg=-257):
        rp = opts["rp"]["id"]
        cd = json.dumps({"type": "webauthn.create", "challenge": opts["challenge"], "origin": origin}).encode()
        n = self.n if n is None else n
        e = self.e if e is None else e
        cose = cbor({1: 3, 3: alg, -1: n.to_bytes((n.bit_length() + 7) // 8, "big"), -2: e.to_bytes((e.bit_length() + 7) // 8, "big")})
        ad = hashlib.sha256(rp.encode()).digest() + bytes([0x45]) + (0).to_bytes(4, "big") + bytes(16) + len(self.cid).to_bytes(2, "big") + self.cid + cose
        return {"clientDataJSON": b64u(cd), "attestationObject": b64u(cbor({"fmt": "none", "attStmt": {}, "authData": ad}))}

    def get(self, opts, origin, tamper=False):
        self.count += 1
        cd = json.dumps({"type": "webauthn.get", "challenge": opts["challenge"], "origin": origin}).encode()
        ad = hashlib.sha256(opts["rpId"].encode()).digest() + bytes([0x05]) + self.count.to_bytes(4, "big")
        open(self.dir + "/m", "wb").write(ad + hashlib.sha256(cd).digest())
        sig = subprocess.run(["openssl", "dgst", "-sha256", "-sign", self.dir + "/k.pem", self.dir + "/m"], capture_output=True, check=True).stdout
        if tamper:
            sig = sig[:-1] + bytes([sig[-1] ^ 1])
        return {"clientDataJSON": b64u(cd), "authenticatorData": b64u(ad), "signature": b64u(sig)}


def test_webauthn_rs256():
    home = tempfile.mkdtemp()
    a = Auth(Store(home))
    origin, rp = "https://hub.tail1234.ts.net", "hub.tail1234.ts.net"
    code = a.mint_code()
    dev = RsaAuthenticator()
    b = a.register_begin(code, rp, "1.1.1.1")
    check("rs256: registration offers alg -257", {"type": "public-key", "alg": -257} in b["publicKey"]["pubKeyCredParams"])
    small = RsaAuthenticator(bits=1024)
    check("rs256: 1024-bit modulus refused", refuses(a.register_finish, b["ticket"], small.create(b["publicKey"], origin), "x", "", "1.1.1.1"))
    b = a.register_begin(code, rp, "1.1.1.1")
    check("rs256: e=3 refused", refuses(a.register_finish, b["ticket"], dev.create(b["publicKey"], origin, e=3), "x", "", "1.1.1.1"))
    b = a.register_begin(code, rp, "1.1.1.1")
    check("rs256: even modulus refused", refuses(a.register_finish, b["ticket"], dev.create(b["publicKey"], origin, n=dev.n + 1), "x", "", "1.1.1.1"))
    b = a.register_begin(code, rp, "1.1.1.1")
    check("rs256: unknown alg refused", refuses(a.register_finish, b["ticket"], dev.create(b["publicKey"], origin, alg=-258), "x", "", "1.1.1.1"))
    b = a.register_begin(code, rp, "1.1.1.1")
    tok = a.register_finish(b["ticket"], dev.create(b["publicKey"], origin), "Windows Hello", "ua", "1.1.1.1")
    check("rs256: enrollment yields a strong session", bool(a.session(tok)) and not a.session(tok)[1]["weak"])
    lb = a.login_begin(rp)
    cred = a.assert_finish(lb["ticket"], b64u(dev.cid), dev.get(lb["publicKey"], origin), "login", "1.1.1.1")
    check("rs256: OpenSSL-made assertion accepted", cred["count"] == 1)
    lb = a.login_begin(rp)
    check("rs256: flipped signature bit refused", refuses(a.assert_finish, lb["ticket"], b64u(dev.cid), dev.get(lb["publicKey"], origin, tamper=True), "login", "1.1.1.1"))
    lb = a.login_begin(rp)
    r = dev.get(lb["publicKey"], origin)
    sig = int.from_bytes(webauthn.unb64u(r["signature"]), "big") + dev.n      # s + n is congruent but not canonical
    r["signature"] = b64u(sig.to_bytes((dev.n.bit_length() + 7) // 8 + 1, "big"))
    check("rs256: non-canonical signature (s + n) refused", refuses(a.assert_finish, lb["ticket"], b64u(dev.cid), r, "login", "1.1.1.1"))
    lb = a.login_begin(rp)
    other = RsaAuthenticator(); other.cid = dev.cid; other.count = 5
    check("rs256: signature from another RSA key refused", refuses(a.assert_finish, lb["ticket"], b64u(dev.cid), other.get(lb["publicKey"], origin), "login", "1.1.1.1"))
    lb = a.login_begin(rp)
    es = Authenticator(); es.cid = dev.cid; es.count = 5
    check("rs256: ES256 signature against an RSA credential refused", refuses(a.assert_finish, lb["ticket"], b64u(dev.cid), es.get(lb["publicKey"], origin), "login", "1.1.1.1"))
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



def fake_composio(accounts):
    """A stand-in for Composio's API; records the keys it was called with."""
    import http.server, threading
    seen = {"keys": [], "deleted": [], "exec": []}

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a): pass

        def reply(self, obj, code=200):
            b = json.dumps(obj).encode()
            self.send_response(code); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)

        def do_GET(self):
            seen["keys"].append(self.headers.get("x-api-key"))
            if self.headers.get("x-api-key") != "ck_good_key_123":
                return self.reply({"error": "no"}, 401)
            if self.path.startswith("/toolkits"):
                return self.reply({"items": [{"slug": "gmail", "name": "Gmail", "meta": {"categories": [{"name": "email"}]}}, {"slug": "Bad Slug!", "name": "x"}]})
            if self.path.startswith("/tools"):
                return self.reply({"items": [{"slug": "GMAIL_FETCH_EMAILS", "description": "read"}, {"slug": "GMAIL_SEND_EMAIL", "description": "send"}, {"slug": "GMAIL_DELETE_MESSAGE", "description": "del"}]})
            if self.path.startswith("/connected_accounts"):
                return self.reply({"items": [{"id": a, "toolkit": {"slug": "gmail"}, "status": "active"} for a in accounts if a not in seen["deleted"]]})
            self.reply({}, 404)

        def do_POST(self):
            seen["keys"].append(self.headers.get("x-api-key"))
            n = int(self.headers.get("Content-Length") or 0); raw = self.rfile.read(n)
            if self.path.startswith("/tools/execute/"):
                seen["exec"].append((self.path.rsplit("/", 1)[1], json.loads(raw or b"{}")))
                return self.reply({"successful": True, "data": {"messages": 2}})
            if self.path == "/tool_router/session":
                return self.reply({"session_id": "sess_1"})
            if self.path == "/tool_router/session/sess_1/link":
                return self.reply({"redirect_url": "https://connect.example/oauth/abc"})
            self.reply({}, 404)

        def do_DELETE(self):
            seen["deleted"].append(self.path.rsplit("/", 1)[1]); self.reply({"ok": True})

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, seen


def test_classify():
    from hub.integrations import classify
    cases = {("gmail", "GMAIL_FETCH_EMAILS"): "read", ("github", "GITHUB_LIST_REPOSITORY_ISSUES"): "read", ("slack", "SLACK_SEARCH_MESSAGES"): "read",
             ("gmail", "GMAIL_SEND_EMAIL"): "write", ("github", "GITHUB_CREATE_AN_ISSUE_COMMENT"): "write", ("gmail", "GMAIL_DELETE_MESSAGE"): "admin",
             ("github", "GITHUB_DELETE_A_REPOSITORY"): "admin", ("github", "GITHUB_GET_OR_CREATE_THING"): "write", ("notion", "NOTION_FROBNICATE"): "write",
             ("gmail", "GITHUB_LIST_ISSUES"): None, ("gmail", "gmail_fetch_emails"): "read", ("gmail", "GMAIL_"): None, ("gmail", "../x"): None}
    for (tk, tool), want in cases.items():
        check(f"classify {tool} on {tk} -> {want}", classify(tk, tool) == want, str(classify(tk, tool)))

def test_http():
    mcp = os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp")
    if not mcp:
        print("SKIP http tests (playbox-mcp not found)")
        return
    home, port = tempfile.mkdtemp(prefix="pbhub-"), free_port()
    fsrv, fseen = fake_composio(["ca_1", "ca_2"])
    fakecli = os.path.join(home, "fake-playbox")
    open(fakecli, "w").write('#!/bin/sh\necho \'{"version":1,"at":%d,"playbox":"9.9.9","score":80,"checks":[{"group":"machine","id":"x","label":"Keys","level":"warn","detail":"too open","fix":"chmod 700","fixable":true}],"fuzz":null}\'\n' % int(time.time()))
    os.chmod(fakecli, 0o755)
    p = subprocess.Popen([sys.executable, os.path.join(UI, "playbox_hub.py"), "--port", str(port), "--home", home, "--mcp", mcp],
                         stdout=subprocess.PIPE, text=True,
                         env={**os.environ, "PLAYBOX_CLI": fakecli, "PLAYBOX_HOME": os.path.join(home, "pbhome"), "PLAYBOX_HUB_NO_MACHINE": "1", "PLAYBOX_HUB_FRESH": "2", "PLAYBOX_HOSTING_DIR": os.path.join(home, "hosting"), "PLAYBOX_COMPOSIO_BASE": "http://127.0.0.1:%d" % fsrv.server_address[1]})
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
        st, j, _ = a.req("GET", "/api/system")
        check("system: the machine report is empty until a check has run", st == 200 and j["machine"]["report"] is None and j["machine"]["running"] is False)
        check("system: a re-check needs a session", Client(port).req("POST", "/api/system/machine", {})[0] == 401)
        st, j, _ = a.req("POST", "/api/system/machine", {})
        check("system: re-check starts", st == 200)
        for _ in range(40):
            j = a.req("GET", "/api/system")[1]
            if j["machine"]["report"]:
                break
            time.sleep(0.25)
        mr = j["machine"]["report"] or {}
        check("system: the CLI's report is shown with its age", mr.get("playbox") == "9.9.9" and mr["checks"][0]["label"] == "Keys" and j["machine"]["age"] is not None and not j["machine"]["stale"], str(j["machine"]))
        check("system: fixing permissions needs a session", Client(port).req("POST", "/api/system/machine/fix", {})[0] == 401)
        st, j2, _ = a.req("POST", "/api/system/machine", {})
        check("system: a second re-check right away is ignored, not queued", st == 200 and j2["machine"]["running"] is False)

        st, j, _ = a.req("GET", "/api/integrations")
        check("integrations: no key yet", st == 200 and j["configured"] is False and j["accounts"] == [])
        check("integrations: a short key is refused", a.req("POST", "/api/integrations/key", {"key": "x"})[0] == 400)
        check("integrations: toolkits need a key", a.req("GET", "/api/integrations/toolkits")[0] == 409)
        a.req("POST", "/api/integrations/key", {"key": "ck_bad_key_1234"})
        check("integrations: a bad key is reported, not leaked", (lambda r: r[0] == 502 and "ck_bad" not in json.dumps(r[1]))(a.req("GET", "/api/integrations/toolkits")))
        st, j, _ = a.req("POST", "/api/integrations/key", {"key": "ck_good_key_123"})
        check("integrations: key saved, only a hint comes back", st == 200 and j["configured"] and j["hint"] == "…_123" and "ck_good" not in json.dumps(j))
        check("integrations: key file is private", oct(os.stat(os.path.join(home, "composio.key")).st_mode & 0o777) == "0o600")
        st, j, _ = a.req("GET", "/api/integrations/toolkits")
        check("integrations: toolkits listed, bad slugs dropped", st == 200 and [t["slug"] for t in j["toolkits"]] == ["gmail"] and j["toolkits"][0]["cats"] == ["email"])
        st, j, _ = a.req("POST", "/api/integrations/connect", {"toolkit": "gmail"})
        check("integrations: connect returns the provider's sign-in page", st == 200 and j["url"] == "https://connect.example/oauth/abc")
        check("integrations: connect refuses a bad toolkit name", a.req("POST", "/api/integrations/connect", {"toolkit": "../x"})[0] == 400)
        st, j, _ = a.req("GET", "/api/integrations")
        check("integrations: connected accounts listed", st == 200 and [x["id"] for x in j["accounts"]] == ["ca_1", "ca_2"] and j["accounts"][0]["toolkit"] == "gmail")
        st, j, _ = a.req("POST", f"/api/rooms/{rid}/grants", {"account": "ca_1", "level": "read"})
        gid = j.get("grant", {}).get("id")
        check("integrations: grant an account to a room", st == 200 and gid and j["grant"]["level"] == "read" and j["grant"]["toolkit"] == "gmail")
        st, j, _ = a.req("POST", f"/api/rooms/{rid}/grants", {"account": "ca_1", "level": "admin"})
        check("integrations: granting again changes the level", st == 200 and j["grant"]["id"] == gid and len(a.req("GET", "/api/integrations")[1]["grants"]) == 1)
        check("integrations: unknown level refused", a.req("POST", f"/api/rooms/{rid}/grants", {"account": "ca_1", "level": "root"})[0] == 400)
        check("integrations: an account you haven't connected is refused", a.req("POST", f"/api/rooms/{rid}/grants", {"account": "ca_999", "level": "read"})[0] == 404)
        check("integrations: unknown room refused", a.req("POST", "/api/rooms/zzzz/grants", {"account": "ca_1", "level": "read"})[0] == 404)
        check("integrations: disconnecting removes its grants", a.req("DELETE", "/api/integrations/accounts/ca_1")[0] == 200 and "ca_1" in fseen["deleted"] and a.req("GET", "/api/integrations")[1]["grants"] == [])
        st, j, _ = a.req("POST", f"/api/rooms/{rid}/grants", {"account": "ca_2", "level": "write"})
        check("integrations: revoke a grant", a.req("DELETE", f"/api/grants/{j['grant']['id']}")[0] == 200 and a.req("GET", "/api/integrations")[1]["grants"] == [])
        check("integrations: only the key header carries the secret", set(fseen["keys"]) <= {"ck_good_key_123", "ck_bad_key_1234"})
        # --- the enforcing proxy ---
        a.req("POST", "/api/integrations/key", {"key": "ck_good_key_123"})
        st, j, _ = a.req("POST", f"/api/rooms/{rid}/agent-tokens", {"label": "claude"})
        tok = j.get("token", "")
        check("proxy: mint an agent key (shown once)", st == 200 and tok.startswith("pbx_") and j["id"] in tok)
        check("proxy: the key is stored hashed", tok.split(".", 1)[1] not in open(os.path.join(home, "hub.json")).read())
        check("proxy: state lists the key without its secret", (lambda t: t and tok.split(".", 1)[1] not in json.dumps(t))(a.req("GET", "/api/integrations")[1]["tokens"]))
        ag = Client(port)
        H = {"Authorization": "Bearer " + tok}
        check("proxy: no key -> 401", ag.req("GET", "/x/grants")[0] == 401)
        check("proxy: a wrong key -> 401", ag.req("GET", "/x/grants", headers={"Authorization": "Bearer pbx_" + "0" * 12 + ".x" * 20})[0] == 401)
        check("proxy: no grant yet -> empty", ag.req("GET", "/x/grants", headers=H)[1]["grants"] == [])
        st, j, _ = ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GMAIL_FETCH_EMAILS", "arguments": {}}, H)
        check("proxy: no grant -> refused", st == 403 and not fseen["exec"])
        gid2 = a.req("POST", f"/api/rooms/{rid}/grants", {"account": "ca_2", "level": "read"})[1]["grant"]["id"]
        st, j, _ = ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GMAIL_FETCH_EMAILS", "arguments": {"max": 1}}, H)
        check("proxy: read grant runs a read tool", st == 200 and j["successful"] and fseen["exec"][-1][0] == "GMAIL_FETCH_EMAILS", str(j))
        ex = fseen["exec"][-1][1]
        check("proxy: the account and user come from the grant, not the agent", ex["connected_account_id"] == "ca_2" and ex["user_id"].startswith("playbox-") and ex["arguments"] == {"max": 1})
        n = len(fseen["exec"])
        check("proxy: read grant refuses a write tool", ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GMAIL_SEND_EMAIL", "arguments": {}}, H)[0] == 403 and len(fseen["exec"]) == n)
        check("proxy: read grant refuses an admin tool", ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GMAIL_DELETE_MESSAGE", "arguments": {}}, H)[0] == 403 and len(fseen["exec"]) == n)
        check("proxy: a tool from another toolkit is refused", ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GITHUB_LIST_ISSUES", "arguments": {}}, H)[0] == 400)
        check("proxy: an agent can't name the account", (ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GMAIL_FETCH_EMAILS", "arguments": {}, "account": "ca_1"}, H)[0] == 200) and fseen["exec"][-1][1]["connected_account_id"] == "ca_2")
        st, j, _ = ag.req("GET", "/x/tools?toolkit=gmail", headers=H)
        allowed = {t["slug"]: t["allowed"] for t in j.get("tools", [])}
        check("proxy: tool list says what this room may call", st == 200 and allowed == {"GMAIL_FETCH_EMAILS": True, "GMAIL_SEND_EMAIL": False, "GMAIL_DELETE_MESSAGE": False}, str(allowed))
        a.req("POST", f"/api/rooms/{rid}/grants", {"account": "ca_2", "level": "write"})
        check("proxy: raising the grant allows writes at once", ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GMAIL_SEND_EMAIL", "arguments": {"to": "a@b.c"}}, H)[0] == 200)
        check("proxy: write still isn't admin", ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GMAIL_DELETE_MESSAGE", "arguments": {}}, H)[0] == 403)
        log = a.req("GET", "/api/integrations")[1]["audit"]
        check("proxy: every call is logged, denials included", any(x["out"] == "ok" for x in log) and any(x["out"].startswith("denied") for x in log) and all("arguments" not in x for x in log))
        a.req("DELETE", f"/api/grants/{gid2}")
        check("proxy: revoking the grant stops the agent", ag.req("POST", "/x/execute", {"toolkit": "gmail", "tool": "GMAIL_FETCH_EMAILS", "arguments": {}}, H)[0] == 403)
        check("proxy: revoking the key stops the agent", a.req("DELETE", f"/api/agent-tokens/{j['id'] if False else tok.split('.')[0][4:]}")[0] == 200 and ag.req("GET", "/x/grants", headers=H)[0] == 401)
        check("proxy: only a session can mint keys", Client(port).req("POST", f"/api/rooms/{rid}/agent-tokens", {})[0] == 401)
        check("integrations: remove the key", a.req("DELETE", "/api/integrations/key")[0] == 200 and not a.req("GET", "/api/integrations")[1]["configured"])
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
        check("host the room from the hub " + json.dumps(j)[:400], st == 200 and not j.get("error"))
        inv = lambda body, c=a, h=None: c.req("POST", f"/api/rooms/{rid}/invite", body, h)
        check("invite needs a session", inv({"ttl": 3600, "grant": 86400, "uses": 1}, anon)[0] == 401)
        check("invite refuses odd durations", inv({"ttl": 12345, "grant": 86400, "uses": 1})[0] == 400 and inv({"ttl": 3600, "grant": 1, "uses": 1})[0] == 400)
        check("invite refuses a malformed guest id", inv({"ttl": 3600, "grant": 86400, "uses": 1, "guest": "xyz"})[0] == 400)
        st, j, _ = inv({"ttl": 3600, "grant": 86400, "uses": 1, "from": "Maya", "approve": False})
        iid, ilink = j.get("id"), j.get("link", "")
        check("invite mints a share url and a playbox link", st == 200 and ilink.startswith("playbox://") and j["url"].endswith("/i/" + (iid or "?")) and j["public"] is False)
        st, j2, _ = inv({"ttl": 3600, "grant": 86400, "uses": 1, "approve": False}, a, {"Host": "93.184.216.34:80", "Origin": "http://93.184.216.34:80"})
        check("on a public address the url is marked reachable" + ("" if st == 200 else f" [{st} {j2}]"), st == 200 and j2["public"] is True and j2["url"].startswith("http://93.184.216.34:80/i/"))
        # ---- the System view: stages, events and posture, with nothing secret in them ----
        check("system needs a session", anon.req("GET", "/api/system")[0] == 401)
        st, sysj, _ = a.req("GET", "/api/system")
        check("system lists the eight stages in order", st == 200 and [x["id"] for x in sysj["stages"]] == ["edge", "auth", "invite", "knock", "approve", "join", "feed", "history"])
        check("system counts the invites just made", sysj["live"]["invite"]["count"] >= 2 and any(e["stage"] == "invite" for e in sysj["events"]))
        check("a key invite is flagged as a warning, an approval invite is not", any(e["stage"] == "invite" and e["level"] == "warn" for e in sysj["events"]))
        check("posture has the core checks", {"edge", "passkey", "weak", "stepup", "invites", "knocks", "chain", "push"} <= {c["id"] for c in sysj["posture"]})
        blob = json.dumps(sysj)
        check("system never carries a link, a key or a room secret", "playbox://" not in blob and ilink[10:60] not in blob)
        st, cv, _ = a.req("GET", f"/r/{rid}/api/canvas")
        check("canvas: a hosted room returns its model with admin rights", st == 200 and cv["admin"] is True and {"members", "feed", "artifacts", "cards", "metrics", "changelog", "stats"} <= set(cv))
        check("canvas needs a session", anon.req("GET", f"/r/{rid}/api/canvas")[0] in (401, 303))
        check("canvas page is served", a.req("GET", f"/r/{rid}/canvas")[0] == 200)
        # ---- adopting a room hosted on this machine ----
        pbx = shutil.which("playbox")
        if not pbx:
            print("SKIP adopt tests (playbox not found)")
        else:
            sd = os.path.join(home, "adoptstate")
            os.makedirs(sd)
            ahost = subprocess.Popen([pbx, "up", "--local", "--console", "--state", sd], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                     env={**os.environ, "PLAYBOX_HOME": os.path.join(home, "hosthome"), "PLAYBOX_HOSTING_DIR": os.path.join(home, "hosting")})
            try:
                for _ in range(80):
                    if os.path.isdir(os.path.join(home, "hosting")) and any(f.endswith(".sock") for f in os.listdir(os.path.join(home, "hosting"))):
                        break
                    time.sleep(0.25)
                st, lj, _ = a.req("GET", "/api/local-rooms")
                check("adopt: a room hosted here is listed", st == 200 and len(lj["rooms"]) == 1 and lj["rooms"][0]["name"] == "adoptstate" and not lj["rooms"][0]["adopted"])
                check("adopt: needs a session", anon.req("GET", "/api/local-rooms")[0] == 401 and anon.req("POST", "/api/local-rooms/adopt", {"id": "0" * 16})[0] == 401)
                check("adopt: an unknown or malformed id is refused", a.req("POST", "/api/local-rooms/adopt", {"id": "0" * 16})[0] == 404 and a.req("POST", "/api/local-rooms/adopt", {"id": "../../x"})[0] == 404)
                hid = lj["rooms"][0]["id"]
                st, aj, _ = a.req("POST", "/api/local-rooms/adopt", {"id": hid, "label": "Adopted"})
                check("adopt: the hub joins the room", st == 200 and bool(aj.get("id")))
                check("adopt: it then shows as adopted", a.req("GET", "/api/local-rooms")[1]["rooms"][0]["adopted"] is True)
                check("adopt: it is in the hub's room list", any(r["id"] == aj["id"] and r["label"] == "Adopted" for r in a.req("GET", "/api/rooms")[1]["rooms"]))
                st, cv, _ = a.req("GET", f"/r/{aj['id']}/api/canvas")
                check("adopt: its canvas loads", st == 200 and cv["room"]["label"] == "Adopted")
            finally:
                try:
                    ahost.stdin.write(b"quit\n")
                    ahost.stdin.flush()
                    ahost.wait(timeout=10)
                except (OSError, subprocess.TimeoutExpired):
                    ahost.kill()
            check("adopt: a host that quits is no longer listed", a.req("GET", "/api/local-rooms")[1]["rooms"] == [])
        st, sysj2, _ = a.req("POST", "/api/system/verify")
        check("verify re-runs the history check", st == 200 and any("Re-verification" in e["text"] for e in sysj2["events"]))
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
        a.req("POST", "/api/system/verify")      # refresh the room digests now instead of waiting for the idle poll
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
        # A room with no repository (talk and notes only) hosts, mints invites, and is remembered like any other.
        st, j4, _ = a.req("POST", "/api/rooms", {"label": "Chat only"})
        rid2 = j4["id"]
        st, j, _ = a.req("POST", f"/r/{rid2}/api/tool", {"name": "playbox_create", "args": {"local": True}})
        check("a room can be hosted with no repository", st == 200 and not j.get("error"), str(j))
        st, j, _ = a.req("POST", f"/api/rooms/{rid2}/invite", {"ttl": 3600, "grant": 86400, "uses": 1, "approve": False})
        check("a room with no repository mints invites", st == 200 and j.get("link", "").startswith("playbox://"), str(j))
        st, j, _ = a.req("POST", f"/api/rooms/{rid2}/invite", {"ttl": 3600, "grant": 86400, "uses": 1})
        check("and approval invites", st == 200 and j.get("approve") is True, str(j))
        saved = json.load(open(os.path.join(home, "hub.json")))
        rec2 = next(x for x in saved["rooms"] if x["id"] == rid2)
        check("the hub remembers it hosts the room (so a restart hosts it again)", rec2.get("host") == {"local": True}, str(rec2.get("host")))
        a.req("DELETE", f"/api/rooms/{rid2}")

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
        pub = lambda m, path, body=None: c.req(m, path, body, {"Host": "playbox.example.com", "Origin": "https://playbox.example.com", "Cf-Connecting-Ip": "203.0.113.8"})
        check("friend: invite page opens without Access", pub("GET", "/i/abc")[0] == 200)
        check("friend: invite status answers without Access", pub("GET", "/api/invite/abc")[0] == 404)
        check("friend: knock reaches the hub without Access", pub("POST", "/api/invite/abc/knock", {"k": "x"})[0] == 404)
        check("friend: icon opens without Access", pub("GET", "/icon.svg")[0] == 200)
        check("friend: everything else still needs Access", all(pub(m, x)[0] == 403 for m, x in
              (("GET", "/api/rooms"), ("GET", "/api/invites"), ("POST", "/api/rooms/x/invite"), ("GET", "/i/a/b"), ("GET", "/api/invite/a/knock"), ("POST", "/api/invite/a/approve"), ("GET", "/"))))
        st, j, _ = via(mint())
        check("tunnelled request with a valid token reaches the hub", st == 200 and j["weak_ok"] is False)
        check("local requests on loopback do not need a token", c.req("GET", "/api/auth/state")[0] == 200)
        # a refused POST must not leave its body on the keep-alive connection (the next request would be parsed from it: 501)
        hc = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        hh = {"Host": "playbox.example.com", "Origin": "https://playbox.example.com", "Cf-Connecting-Ip": "203.0.113.9", "Content-Type": "application/json"}
        hc.request("POST", "/api/rooms/x/invite", json.dumps({"approve": True, "pad": "x" * 500}), hh)
        r1 = hc.getresponse(); r1.read()
        hc.request("POST", "/api/auth/stepup/begin", "{}", hh)
        r2 = hc.getresponse(); r2.read()
        hc.close()
        check("a refused POST body does not poison the next request on the connection", r1.status == 403 and r2.status == 403)
    finally:
        p.terminate()
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
        shutil.rmtree(d, ignore_errors=True)


def test_tunnel_keeper():
    """The tunnel is retried in the background: a failed start, then a dead tunnel, each ends with a live one again."""
    sys.path.insert(0, UI)
    from hub.server import keep_tunnel

    class P:
        def __init__(self):
            self.dead, self.killed = False, False
        def poll(self):
            return 1 if self.dead else None
        def terminate(self):
            self.killed = True

    starts, procs = [], []
    stop = threading.Event()
    opts = {}

    def start():
        starts.append(1)
        if len(starts) <= 2:
            return None, None                       # the network is down
        p = P()
        procs.append(p)
        return p, f"https://t{len(starts)}.example"
    delays = []
    ticks = {"n": 0}

    def wait(t):
        delays.append(t)
        ticks["n"] += 1
        if len(procs) == 1 and ticks["n"] > 6 and not procs[0].dead:
            procs[0].dead = True                    # cloudflared dies after a while
        if len(procs) == 2 and ticks["n"] > 9:
            stop.set()
        return stop.is_set()
    state = {"proc": None}
    keep_tunnel(state, opts, start, stop, wait=wait, first=5.0, cap=60.0)
    check("tunnel: a failed first start is retried with backoff", len(starts) >= 3 and delays[:2] == [5.0, 10.0])
    check("tunnel: the public address is set once it connects", opts.get("public_url", "").startswith("https://t"))
    check("tunnel: a tunnel that dies is started again", len(procs) == 2 and opts["public_url"] == f"https://t{len(starts)}.example")


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
        cfgp = agents._desktop_cfg(agents.Path(h))
        os.makedirs(cfgp.parent)
        open(cfgp, "w").write(json.dumps({"mcpServers": {"keep": {"command": "x"}}, "other": 1}))
        check("claude desktop is installed, not yet connected", {a["id"]: a for a in agents.detect()}["desktop"]["installed"] and not {a["id"]: a for a in agents.detect()}["desktop"]["connected"])
        ok, out = agents.connect("desktop")
        cfg = json.load(open(cfgp))
        check("connecting claude desktop adds playbox and keeps the rest", ok and cfg["mcpServers"]["playbox"] == {"command": fake, "args": ["mcp"]} and "keep" in cfg["mcpServers"] and cfg["other"] == 1)
        check("claude desktop now reads as connected", {a["id"]: a for a in agents.detect()}["desktop"]["connected"])
        check("manual snippet is a stdio MCP server", json.loads(agents.manual()["json"])["mcpServers"]["playbox"]["args"] == ["mcp"])
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


def spawn_hub(mcp, tag):
    home, port = tempfile.mkdtemp(prefix=f"pbhub-{tag}-"), free_port()
    p = subprocess.Popen([sys.executable, os.path.join(UI, "playbox_hub.py"), "--port", str(port), "--home", home, "--mcp", mcp],
                         stdout=subprocess.PIPE, text=True, env={**os.environ, "PLAYBOX_HUB_NO_MACHINE": "1", "PLAYBOX_HUB_FRESH": "2"})
    code = None
    for _ in range(12):
        m = re.search(r"/pair#(\S+)", p.stdout.readline())
        if m:
            code = m.group(1)
            break
    c = Client(port)
    st, j, _ = c.req("POST", "/api/auth/weak", {"code": code, "name": tag})
    return p, c, port, home


def test_approval():
    """Ask-to-join across two real hubs: knock, verify code, approval, a link bound to the asker, deny, limits."""
    mcp = os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp")
    if not mcp:
        print("SKIP approval tests (playbox-mcp not found)")
        return
    sys.path.insert(0, UI)
    from playbox_ui import Sidecar
    from hub.knock import verify_code
    hp, host, hport, hhome = spawn_hub(mcp, "host")
    fp, friend, fport, fhome = spawn_hub(mcp, "friend")
    extra = []
    try:
        repo = os.path.join(hhome, "repo")
        os.makedirs(repo)
        subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
        subprocess.run(["git", "-c", "user.email=a@b", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"], cwd=repo, check=True)
        rid = host.req("POST", "/api/rooms", {"label": "Lab", "kind": "mine"})[1]["id"]
        host.req("POST", f"/r/{rid}/api/tool", {"name": "playbox_create", "args": {"repo": repo, "local": True}})
        st, inv, _ = host.req("POST", f"/api/rooms/{rid}/invite", {"ttl": 3600, "grant": 86400, "uses": 2, "from": "Shaan"})
        iid, k = inv.get("id"), inv.get("k")
        check("an approval invite carries a secret but no Playbox key", st == 200 and inv.get("approve") is True and k and "link" not in inv)
        check("the share page says it needs approval", host.req("GET", "/api/invite/" + iid)[1].get("approve") is True)
        anon = Client(hport)
        kn = lambda body, c=anon: c.req("POST", f"/api/invite/{iid}/knock", body)
        g0 = "ab" * 32
        check("a knock with the wrong secret is refused", kn({"k": "nope", "guest": g0, "name": "x"})[0] == 403)
        check("a knock with a malformed guest id is refused", kn({"k": k, "guest": "xyz", "name": "x"})[0] == 400)
        check("knocking on an unknown invite is a 404", anon.req("POST", "/api/invite/nope/knock", {"k": k, "guest": g0, "name": "x"})[0] == 404)

        base = f"http://127.0.0.1:{hport}"
        st, j, _ = friend.req("POST", "/api/knock", {"base": base, "id": iid, "k": k, "label": "Lab", "name": "Maya"})
        code, froom = j.get("code"), j.get("room")
        check("the friend's hub knocks and gets a verify code", st == 200 and code and froom, str(j))
        st, reqs, _ = host.req("GET", "/api/knocks")
        r0 = (reqs.get("requests") or [{}])[0]
        check("the host sees the request with the same code and the asker's name", len(reqs["requests"]) == 1 and r0["code"] == code and r0["name"] == "Maya" and r0["label"] == "Lab", str(reqs))
        att = host.req("GET", "/api/attention")[1]["items"]
        check("a join request is an urgent attention item", any(a["kind"] == "knock" and a["urgent"] and code in a["body"] for a in att))
        out = friend.req("GET", "/api/knock/out")[1]["requests"]
        check("until approved the friend is waiting, not in the room", out[0]["state"] == "waiting" and next(x for x in friend.req("GET", "/api/rooms")[1]["rooms"] if x["id"] == froom)["mode"] == "none")
        check("the key is not in the host's state while pending", "playbox://" not in open(os.path.join(hhome, "hub.json")).read())

        check("anyone can't approve without a session", Client(hport).req("POST", f"/api/knocks/{r0['rid']}/approve", {})[0] == 401)
        st, _, _ = host.req("POST", f"/api/knocks/{r0['rid']}/approve", {})
        check("the host approves", st == 200)
        mode = None
        for _ in range(30):
            mode = next((x["mode"] for x in friend.req("GET", "/api/rooms")[1]["rooms"] if x["id"] == froom), None)
            if mode == "guest":
                break
            time.sleep(1)
        check("the friend's hub is then in the room as a guest", mode == "guest")
        check("the knock key is wiped from the host once handed over", all(x["link"] is None for x in json.load(open(os.path.join(hhome, "hub.json")))["knocks"].values()))

        # an agent asks too: the link that comes back is bound to the agent's own id
        agent = Sidecar(mcp, os.path.join(fhome, "agent"))
        stranger = Sidecar(mcp, os.path.join(fhome, "stranger"))
        extra += [agent, stranger]
        gid = agent.call("playbox_status", {})[1]["guest_id"]
        st, j, _ = friend.req("POST", "/api/knock", {"base": base, "id": iid, "k": k, "label": "Lab", "name": "Maya's agent", "agent_guest": gid})
        check("an agent can ask with its own guest id", st == 200 and j["code"] == verify_code(iid, gid) and j["room"] is None, str(j))
        r1 = host.req("GET", "/api/knocks")[1]["requests"][0]
        host.req("POST", f"/api/knocks/{r1['rid']}/approve", {})
        link = None
        for _ in range(30):
            row = friend.req("GET", "/api/knock/out")[1]["requests"]
            link = next((x.get("link") for x in row if x["agent"] and x["state"] == "approved"), None)
            if link:
                break
            time.sleep(1)
        check("the approved link comes back to the friend's hub for the agent", bool(link))
        check("a different identity cannot use that link", bool(stranger.call("playbox_join", {"link": link})[0]))
        check("the asker's identity can", not agent.call("playbox_join", {"link": link})[0])
        st, _, _ = kn({"k": k, "guest": "cd" * 32, "name": "Late"})
        check("an invite that has let in everyone it was for takes no more requests", st == 410)

        # deny
        st, inv2, _ = host.req("POST", f"/api/rooms/{rid}/invite", {"ttl": 3600, "grant": 86400, "uses": 1, "from": "Shaan"})
        iid2, k2 = inv2["id"], inv2["k"]
        st, j, _ = friend.req("POST", "/api/knock", {"base": base, "id": iid2, "k": k2, "label": "Second", "name": "Eve"})
        r2 = host.req("GET", "/api/knocks")[1]["requests"][0]
        check("deny works without a passkey touch", host.req("POST", f"/api/knocks/{r2['rid']}/deny", {})[0] == 200)
        state = None
        for _ in range(20):
            state = next((x["state"] for x in friend.req("GET", "/api/knock/out")[1]["requests"] if x["rid"] == j["rid"]), None)
            if state == "denied":
                break
            time.sleep(1)
        check("the friend sees it was denied, and the placeholder room is gone", state == "denied" and not any(x["id"] == j["room"] for x in friend.req("GET", "/api/rooms")[1]["rooms"]))
        check("a host address that isn't https or private is refused", friend.req("POST", "/api/knock", {"base": "http://example.com", "id": iid2, "k": k2, "name": "x"})[0] == 400)
        check("withdrawing an approval invite stops new requests", host.req("DELETE", "/api/invites/" + iid2)[0] == 200 and kn_to(anon, iid2, {"k": k2, "guest": "ef" * 32, "name": "x"})[0] == 410)
    finally:
        for x in extra:
            x.close()
        for p in (hp, fp):
            p.terminate()
        for d in (hhome, fhome):
            shutil.rmtree(d, ignore_errors=True)


def kn_to(c, iid, body):
    return c.req("POST", f"/api/invite/{iid}/knock", body)


def test_board():
    """The room's work surface is built only from feed messages; every field is untrusted and whitelisted."""
    from hub import board
    E = board.encode
    it = lambda n, f, t, ms: {"seq": n, "from": f, "kind": "say", "text": t, "time_ms": ms}
    check("board: only ::pbx lines with a known kind parse", board.parse(E("card", id="a")) is not None and board.parse("hello") is None and board.parse("::pbx evil {}") is None and board.parse("::pbx card {nope") is None)
    items = [it(1, "aa", E("agent", name="Ada", machine="mac", role="prover"), 1000), it(2, "aa", E("artifact", id="p", title="Bound", kind="proof", status="checking", checks=[{"name": "build", "ok": True}]), 2000),
             it(3, "bb", E("artifact", id="p", status="verified"), 3000), it(4, "aa", E("card", id="c", title="X", col="todo"), 4000), it(5, "aa", E("card", id="c", col="done"), 5000),
             it(6, "bb", E("metric", name="K", value=12), 6000), it(7, "bb", E("metric", name="K", value=float("nan")), 7000), it(8, "aa", "plain <b>text</b>", 8000),
             it(9, "cc", E("artifact", id="q", title="<script>alert(1)</script>", kind="malware", status="pwned"), 9000), it(10, "cc", E("card", id="z", title="t", col="hacked"), 10000)]
    m = board.build(items, [{"id": "aa", "present": True}], "aa", 600000)
    a = {x["id"]: x for x in m["artifacts"]}
    check("board: an artifact keeps its revisions and takes the newest status", a["p"]["status"] == "verified" and len(a["p"]["revs"]) == 2 and a["p"]["checks"][0]["ok"])
    check("board: a status change is a changelog line from the real sender", any("from checking to verified" in e["text"] and e["who"] == "bb" for e in m["changelog"]))
    check("board: a card moves between columns", [c["col"] for c in m["cards"] if c["id"] == "c"] == ["done"] and any("from todo to done" in e["text"] for e in m["changelog"]))
    check("board: bad kinds, statuses and columns fall back to safe values", a["q"]["kind"] == "note" and a["q"]["status"] == "draft" and [c["col"] for c in m["cards"] if c["id"] == "z"] == ["todo"])
    check("board: a non-finite metric is dropped", [len(s["pts"]) for s in m["metrics"]] == [1])
    check("board: plain messages stay plain and work messages are not in the message list", [x["text"] for x in m["feed"]] == ["plain <b>text</b>"])
    check("board: the agent announcement names the sender and machine", [x["name"] for x in m["members"] if x["id"] == "aa"] == ["Ada"] and [x["machine"] for x in m["members"] if x["id"] == "aa"] == ["mac"])
    check("board: a claimed owner cannot change who published", a["p"]["by"] == "aa" and a["p"]["last_by"] == "bb")
    check("board: oversized bodies are ignored", board.parse("::pbx card " + "{" + '"x":"' + "a" * 5000 + '"}') is None)


def test_rehost():
    """Hosting lives in the sidecar's memory: a hub that restarts (or respawns a sidecar) must host the room again, or invites stop working."""
    from hub.rooms import Room
    mcp = os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp")
    if not mcp:
        print("SKIP rehost (playbox-mcp not found)")
        return
    d = tempfile.mkdtemp(prefix="pbrh-")
    repo = os.path.join(d, "repo")
    os.makedirs(repo)
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "-c", "user.email=a@b", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "i"], cwd=repo, check=True)
    saved = []
    r = Room({"id": "t1", "label": "t", "created": 0, "kind": "mine", "home": os.path.join(d, "home")}, mcp, d)
    r.on_change = lambda: saved.append(1)
    try:
        sc = r.ensure()
        sc.call("playbox_create", {"repo": repo, "local": True})
        r.tick()
        check("rehost: the hub remembers what a room hosts", r.rec.get("host", {}).get("repo") == repo and saved)
        r.sidecar.proc.kill()
        time.sleep(1)
        _, st = r.ensure().call("playbox_status", {})
        check("rehost: a respawned sidecar hosts the room again", isinstance(st, dict) and (st.get("hosting") or {}).get("repo") == repo, str(r.err))
        r.close()
        r2 = Room({"id": "t2", "label": "chat", "created": 0, "kind": "mine", "home": os.path.join(d, "home2")}, mcp, d)
        try:
            r2.ensure().call("playbox_create", {"local": True})
            r2.tick()
            check("rehost: a room with no repository is remembered too", r2.rec.get("host") == {}, str(r2.rec.get("host")))
            r2.sidecar.proc.kill()
            time.sleep(1)
            _, st2 = r2.ensure().call("playbox_status", {})
            check("rehost: and hosted again after a respawn", isinstance(st2, dict) and bool(st2.get("hosting")), str(r2.err))
        finally:
            r2.close()
    finally:
        r.close()
        shutil.rmtree(d, ignore_errors=True)


if __name__ == "__main__":
    test_crypto()
    test_webauthn()
    test_webauthn_rs256()
    test_push_send()
    test_http()
    test_cloudflare()
    test_access()
    test_tunnel_keeper()
    test_rehost()
    test_board()
    test_classify()
    test_agents()
    test_landing_quoting()
    test_approval()
    print(f"\n{len(fails)} failed" if fails else "\nall passed")
    sys.exit(1 if fails else 0)
