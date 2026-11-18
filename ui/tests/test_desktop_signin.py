#!/usr/bin/env python3
"""Desktop app sign-in and Touch ID / owner step-up bridge tests."""
import hashlib
import http.client
import json
import os
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

UI = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
bad = []


def check(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + ("" if ok or not detail else f"  [{detail}]"))
    if not ok:
        bad.append(name)


def req(port, method, path, body=None, headers=None):
    h = {"Host": f"localhost:{port}"}
    if method != "GET":
        h["Origin"] = f"http://localhost:{port}"
    if body is not None:
        h["Content-Type"] = "application/json"
    if headers:
        h.update(headers)
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
    c.request(method, path, json.dumps(body) if body is not None else None, h)
    r = c.getresponse()
    raw = r.read()
    out = (r.status, raw, r.getheader("Set-Cookie"), r.getheader("Location"))
    c.close()
    return out


def get(port, path, headers=None):
    return req(port, "GET", path, None, headers)


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


def signin_local(port, home, next_q=""):
    n = secrets.token_urlsafe(32)
    drop(home, n)
    st, _, cookie, loc = get(port, "/desktop/signin?n=" + n + (f"&next={next_q}" if next_q else ""))
    assert st == 303 and cookie and cookie.startswith("pbs="), f"signin failed: {st}"
    return cookie.split(";")[0], loc


def write_ready(home, mode=0o600):
    d = os.path.join(home, "desktop-stepup")
    os.makedirs(d, mode=0o700, exist_ok=True)
    os.chmod(d, 0o700)
    f = os.path.join(d, "ready")
    open(f, "w").close()
    os.chmod(f, mode)
    return d, f


def start_watcher(home, behavior):
    """Simulates Playbox Desktop's step-up watcher for one request (`approve`, `wrong_proof`, `bad_mode`, `deny`)."""
    d, _ = write_ready(home)
    stop = threading.Event()
    seen = {}

    def loop():
        deadline = time.monotonic() + 10.0
        while not stop.is_set() and time.monotonic() < deadline:
            try:
                names = [x for x in os.listdir(d) if x.endswith(".req") and not x.endswith(".req.tmp")]
            except OSError:
                names = []
            if not names:
                time.sleep(0.03)
                continue
            rp = os.path.join(d, names[0])
            try:
                q = json.loads(open(rp, "r").read())
            except (OSError, ValueError):
                time.sleep(0.02)
                continue
            seen["req"] = q
            rid, nonce = q["id"], q["nonce"]
            good_proof = hashlib.sha256(("playbox-stepup-v1:" + nonce).encode()).hexdigest()
            if behavior == "approve":
                resp, mode = {"id": rid, "ok": True, "proof": good_proof}, 0o600
            elif behavior == "wrong_proof":
                resp, mode = {"id": rid, "ok": True, "proof": "0" * 64}, 0o600
            elif behavior == "bad_mode":
                resp, mode = {"id": rid, "ok": True, "proof": good_proof}, 0o644
            elif behavior == "deny":
                resp, mode = {"id": rid, "ok": False, "error": "Cancelled by user"}, 0o600
            else:
                return
            tmp = os.path.join(d, f"{rid}.res.tmp")
            res = os.path.join(d, f"{rid}.res")
            fd = os.open(tmp, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, mode)
            os.write(fd, json.dumps(resp).encode())
            os.close(fd)
            os.chmod(tmp, mode)
            os.replace(tmp, res)
            return

    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return stop, t, seen


def main() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    home = tempfile.mkdtemp(prefix="pbdesk-")
    p = subprocess.Popen(
        [sys.executable, os.path.join(UI, "playbox_hub.py"), "--port", str(port), "--home", home, "--mcp", "/usr/bin/true"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        env={**os.environ, "PLAYBOX_HUB_NO_MACHINE": "1", "PLAYBOX_STEPUP_WAIT_S": "1.5"},
    )
    try:
        for _ in range(60):
            try:
                if get(port, "/api/auth/state")[0] == 200:
                    break
            except OSError:
                time.sleep(0.25)

        # ---- Part 1: Desktop sign-in (all 12 core checks + safe next redirect) ----
        st, body, _, _ = get(port, "/api/auth/state")
        check("state advertises local sign-in", st == 200 and json.loads(body).get("local_signin") is True)

        n = secrets.token_urlsafe(32)
        check("no proof file: refused", get(port, "/desktop/signin?n=" + n)[0] == 403)
        f = drop(home, n)
        st, _, cookie, loc = get(port, "/desktop/signin?n=" + n)
        check("proof file: signed in and sent to /", st == 303 and loc == "/" and cookie and cookie.startswith("pbs="))
        check("the proof file is gone (single use)", not os.path.exists(f))
        check("same nonce again: refused", get(port, "/desktop/signin?n=" + n)[0] == 403)
        tok = cookie.split(";")[0]
        st, body, _, _ = get(port, "/api/auth/state", {"Cookie": tok})
        stj = json.loads(body)
        check("the session is authed, weak, and local_only", st == 200 and stj["authed"] is True and stj["weak"] is True and stj["local_only"] is True)
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

        _, loc_ok = signin_local(port, home, "/r/room_1/canvas")
        _, loc_bad = signin_local(port, home, "//evil.example/x")
        check("safe next room redirect works; open-redirect falls back to /", loc_ok == "/r/room_1/canvas" and loc_bad == "/")

        # ---- Part 2: Desktop Step-Up Protocol ----
        # 1. local_only session starts with fresh: false, and sensitive endpoints ask for stepup with local_only: true
        st, raw, _, _ = get(port, "/api/me", {"Cookie": tok})
        me = json.loads(raw)
        check("stepup: local_only session starts with fresh: false", st == 200 and me["local_only"] is True and me["fresh"] is False and me["desktop_bridge"] is False)
        st, raw, _, _ = req(port, "POST", "/api/devices/pair", {}, {"Cookie": tok})
        pj = json.loads(raw)
        check("stepup: sensitive endpoint returns 403 stepup with local_only: true", st == 403 and pj.get("error") == "stepup" and pj.get("local_only") is True and pj.get("desktop_bridge") is False)

        # 2. Without desktop-stepup/ready, POST /api/auth/desktop-stepup fails fast with 403
        t0 = time.monotonic()
        st, raw, _, _ = req(port, "POST", "/api/auth/desktop-stepup", {"reason": "Pair device"}, {"Cookie": tok})
        check("stepup: without desktop-stepup/ready fails fast with 403", st == 403 and (time.monotonic() - t0) < 0.8)

        # 3. Wrong proof hash, world-readable .res (0644), and denied ({"ok": false}) are refused (403) and leave fresh: false
        for label, beh in (("wrong proof hash", "wrong_proof"), ("world-readable .res (0644)", "bad_mode"), ("user denied", "deny")):
            stop, th, _ = start_watcher(home, beh)
            st, raw, _, _ = req(port, "POST", "/api/auth/desktop-stepup", {"reason": "Test " + label}, {"Cookie": tok})
            stop.set()
            th.join(2)
            me_after = json.loads(get(port, "/api/me", {"Cookie": tok})[1])
            leftover = [x for x in os.listdir(os.path.join(home, "desktop-stepup")) if x != "ready"]
            check(f"stepup: {label} is refused (403), leaves fresh: false, cleans files", st == 403 and me_after["fresh"] is False and not leftover, f"st={st} leftover={leftover}")

        # 4. Non-loopback (X-Forwarded-For) request to /api/auth/desktop-stepup is refused
        write_ready(home)
        st_xff, _, _, _ = req(port, "POST", "/api/auth/desktop-stepup", {"reason": "Pair"}, {"Cookie": tok, "X-Forwarded-For": "203.0.113.9"})
        check("stepup: non-loopback (X-Forwarded-For) request is refused", st_xff in (401, 403))

        # 5. Simulated desktop watcher approves -> 200, fresh: true, and POST /api/devices/pair succeeds!
        stop, th, seen = start_watcher(home, "approve")
        st, raw, _, _ = req(port, "POST", "/api/auth/desktop-stepup", {"reason": "Pair iPhone\nignore\x00ctrl"}, {"Cookie": tok})
        stop.set()
        th.join(2)
        me_ok = json.loads(get(port, "/api/me", {"Cookie": tok})[1])
        check("stepup: valid desktop proof succeeds (200) and sanitizes reason", st == 200 and json.loads(raw).get("ok") is True and seen.get("req", {}).get("reason") == "Pair iPhone ignore ctrl")
        check("stepup: /api/me now reports fresh: true and desktop_bridge: true", me_ok["fresh"] is True and me_ok["desktop_bridge"] is True)
        st_pair, raw_pair, _, _ = req(port, "POST", "/api/devices/pair", {}, {"Cookie": tok})
        check("stepup: sensitive endpoint (POST /api/devices/pair) immediately succeeds", st_pair == 200 and bool(json.loads(raw_pair).get("code")))
    finally:
        p.terminate()
        p.wait(10)
        shutil.rmtree(home, ignore_errors=True)
    print(f"{len(bad)} failed" if bad else "all passed")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
