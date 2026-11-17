#!/usr/bin/env python3
"""Desktop app sign-in: a process that can write into the hub's private home opens a one-use, local-only session; nothing else can."""
import hashlib
import http.client
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import time

UI = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
bad = []


def check(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok:
        bad.append(name)


def get(port, path, headers=None):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
    c.request("GET", path, headers={"Host": f"localhost:{port}", **(headers or {})})
    r = c.getresponse()
    body = r.read()
    out = (r.status, body, r.getheader("Set-Cookie"), r.getheader("Location"))
    c.close()
    return out


def drop(home, nonce, mode=0o600, age=0):
    d = os.path.join(home, "desktop-login")
    os.makedirs(d, mode=0o700, exist_ok=True)
    f = os.path.join(d, hashlib.sha256(nonce.encode()).hexdigest())
    open(f, "w").close()
    os.chmod(f, mode)
    if age:
        t = time.time() - age
        os.utime(f, (t, t))
    return f


def main() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    home = tempfile.mkdtemp()
    p = subprocess.Popen([sys.executable, os.path.join(UI, "playbox_hub.py"), "--port", str(port), "--home", home, "--mcp", "/usr/bin/true"],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env={**os.environ, "PLAYBOX_HUB_NO_MACHINE": "1"})
    try:
        for _ in range(60):
            try:
                if get(port, "/api/auth/state")[0] == 200:
                    break
            except OSError:
                time.sleep(0.25)
        st, body, _, _ = get(port, "/api/auth/state")
        check("state advertises local sign-in", st == 200 and b'"local_signin": true' in body)

        n = secrets.token_urlsafe(32)
        check("no proof file: refused", get(port, "/desktop/signin?n=" + n)[0] == 403)
        f = drop(home, n)
        st, _, cookie, loc = get(port, "/desktop/signin?n=" + n)
        check("proof file: signed in and sent to /", st == 303 and loc == "/" and cookie and cookie.startswith("pbs="))
        check("the proof file is gone (single use)", not os.path.exists(f))
        check("same nonce again: refused", get(port, "/desktop/signin?n=" + n)[0] == 403)
        tok = cookie.split(";")[0]
        st, body, _, _ = get(port, "/api/auth/state", {"Cookie": tok})
        check("the session is authed and weak", st == 200 and b'"authed": true' in body and b'"weak": true' in body)
        st, _, _, _ = get(port, "/api/rooms", {"Cookie": tok})
        check("the session can use the API on this machine", st == 200)
        st, _, _, _ = get(port, "/api/rooms", {"Cookie": tok, "X-Forwarded-For": "203.0.113.9"})
        check("the same cookie is nothing behind a proxy header", st == 401)

        n2 = secrets.token_urlsafe(32)
        drop(home, n2, mode=0o644)
        check("a world-readable proof file is refused", get(port, "/desktop/signin?n=" + n2)[0] == 403)
        n3 = secrets.token_urlsafe(32)
        drop(home, n3, age=120)
        check("an old proof file is refused", get(port, "/desktop/signin?n=" + n3)[0] == 403)
        n4 = secrets.token_urlsafe(32)
        drop(home, n4)
        check("a proxied request is refused even with a proof file", get(port, "/desktop/signin?n=" + n4, {"Cf-Connecting-Ip": "203.0.113.9"})[0] == 403)
        check("a foreign Host is refused", get(port, "/desktop/signin?n=" + n4, {"Host": "evil.example"})[0] == 403)
        check("a short nonce is refused", get(port, "/desktop/signin?n=abc")[0] == 403)
    finally:
        p.terminate()
        p.wait(10)
    print(f"{len(bad)} failed" if bad else "all passed")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
