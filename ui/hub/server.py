"""The Playbox hub: one PWA, many rooms, passkey auth, live events, payload-free push."""
import argparse
import hmac
import http.server
import ipaddress
import json
import os
import queue
import secrets
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.parse
from pathlib import Path

from . import push as webpush
from .auth import Auth, AuthError, PROXY_HEADERS, hostname_of
from .icons import png_icon
from .rooms import Hub
from .store import Store

STATIC = Path(__file__).resolve().parent.parent / "static"
MAX_BODY = 1 << 20
SENSITIVE_TOOLS = {"playbox_revoke"}
CSP = ("default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; "
       "connect-src 'self'; manifest-src 'self'; worker-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
PUBLIC_FILES = {"/auth.js": ("auth.js", "text/javascript; charset=utf-8"), "/sw.js": ("sw.js", "text/javascript; charset=utf-8")}


def is_local(peer, headers):
    try:
        return ipaddress.ip_address(peer).is_loopback and not any(h in headers for h in PROXY_HEADERS)
    except ValueError:
        return False


def make_handler(hub, auth, store, opts):
    class H(http.server.BaseHTTPRequestHandler):
        server_version = "playbox-hub"
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        # ---- plumbing ----
        @property
        def peer(self):
            return self.client_address[0]

        def hdrs(self):
            return {k.lower() for k in self.headers.keys()}

        def secure(self):
            return self.headers.get("X-Forwarded-Proto", "").lower() == "https"

        def rp_id(self):
            return hostname_of(self.headers.get("Host", ""))

        def send(self, code, body=b"", ctype="application/json", extra=None, cache="no-store"):
            data = body if isinstance(body, bytes) else body.encode()
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", cache)
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Referrer-Policy", "no-referrer")
            self.send_header("X-Frame-Options", "DENY")
            self.send_header("Content-Security-Policy", CSP)
            self.send_header("Cross-Origin-Opener-Policy", "same-origin")
            for k, v in (extra or {}).items():
                self.send_header(k, v)
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)

        def j(self, code, obj, extra=None):
            self.send(code, json.dumps(obj), extra=extra)

        def body(self):
            if "application/json" not in self.headers.get("Content-Type", ""):
                raise AuthError(415, "json only")
            n = int(self.headers.get("Content-Length", "0") or 0)
            if n > MAX_BODY:
                raise AuthError(413, "too large")
            try:
                v = json.loads(self.rfile.read(n) or b"{}")
            except ValueError:
                raise AuthError(400, "bad json")
            if not isinstance(v, dict):
                raise AuthError(400, "object expected")
            return v

        def origin_ok(self):
            """State-changing requests must come from this very origin (blocks cross-site and DNS-rebinding tricks)."""
            host = self.headers.get("Host", "")
            o = self.headers.get("Origin")
            if o is not None:
                return urllib.parse.urlparse(o).netloc == host
            return self.headers.get("Sec-Fetch-Site") in (None, "same-origin", "none")

        def cookie(self):
            for part in self.headers.get("Cookie", "").split(";"):
                k, _, v = part.strip().partition("=")
                if k == "pbs":
                    return v
            return None

        def set_cookie(self, tok, age=30 * 86400):
            return {"Set-Cookie": f"pbs={tok}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}" + ("; Secure" if self.secure() else "")}

        def authed(self):
            if opts.get("tailnet_login") and self.headers.get("Tailscale-User-Login") != opts["tailnet_login"]:
                return None
            return auth.session(self.cookie())

        def need_fresh(self, rec):
            if auth.is_fresh(rec, is_local(self.peer, self.hdrs())):
                return True
            self.j(403, {"error": "stepup", "weak": rec["weak"]})
            return False

        # ---- dispatch ----
        def do_HEAD(self):
            self.do_GET()

        def do_GET(self):
            try:
                self.route("GET")
            except AuthError as e:
                self.j(e.code, {"error": e.msg})
            except (BrokenPipeError, ConnectionResetError):
                pass

        def do_POST(self):
            self.mutate("POST")

        def do_DELETE(self):
            self.mutate("DELETE")

        def do_PATCH(self):
            self.mutate("PATCH")

        def mutate(self, method):
            try:
                if not self.origin_ok():
                    return self.j(403, {"error": "cross-origin request refused"})
                self.route(method)
            except AuthError as e:
                self.j(e.code, {"error": e.msg})
            except (BrokenPipeError, ConnectionResetError):
                pass

        def route(self, m):
            url = urllib.parse.urlparse(self.path)
            p = url.path
            # public shell
            if m == "GET":
                if p in ("/", "/pair"):
                    return self.send(200, (STATIC / "home.html").read_bytes(), "text/html; charset=utf-8")
                if p in PUBLIC_FILES:
                    f, ct = PUBLIC_FILES[p]
                    return self.send(200, (STATIC / f).read_bytes(), ct, {"Service-Worker-Allowed": "/"} if f == "sw.js" else None)
                if p == "/manifest.webmanifest":
                    return self.send(200, json.dumps(MANIFEST), "application/manifest+json")
                if p == "/icon.svg":
                    return self.send(200, ICON_SVG, "image/svg+xml", cache="public, max-age=86400")
                if p in ("/icon-192.png", "/icon-512.png", "/icon-maskable.png"):
                    return self.send(200, png_icon(192 if "192" in p else 512, "maskable" in p), "image/png", cache="public, max-age=86400")
                if p == "/api/auth/state":
                    rec = self.authed()
                    return self.j(200, {"enrolled": auth.enrolled(), "authed": bool(rec), "weak": bool(rec and rec[1]["weak"]), "rp": self.rp_id(),
                                        "weak_ok": opts["weak"] and auth.weak_allowed(self.peer, self.headers.get("Host"), self.hdrs())})
            if p.startswith("/api/auth/") and m == "POST":
                return self.auth_post(p)
            # everything below needs a session
            sess = self.authed()
            if not sess:
                if m == "GET" and p.startswith("/r/"):
                    return self.send(303, "", "text/plain", {"Location": "/"})
                return self.j(401, {"error": "unauthorized"})
            key, rec = sess
            if p.startswith("/r/"):
                return self.room_route(m, p, key, rec)
            return self.api(m, p, url, key, rec)

        # ---- auth endpoints ----
        def auth_post(self, p):
            ip = self.peer
            auth.throttle(ip)
            b = self.body()
            ua = self.headers.get("User-Agent", "")
            if p == "/api/auth/weak":
                if not (opts["weak"] and auth.weak_allowed(ip, self.headers.get("Host"), self.hdrs())):
                    raise AuthError(403, "plain-HTTP pairing is only allowed on a private network, never through a proxy or tunnel; use HTTPS and a passkey")
                tok = auth.weak_session(str(b.get("code", "")), ua, ip, str(b.get("name", ""))[:40])
                return self.j(200, {"ok": True}, self.set_cookie(tok))
            if p == "/api/auth/register/begin":
                return self.j(200, auth.register_begin(str(b.get("code", "")), self.rp_id(), ip))
            if p == "/api/auth/register/finish":
                tok = auth.register_finish(str(b.get("ticket", "")), b.get("response") or {}, str(b.get("name", "")), ua, ip)
                return self.j(200, {"ok": True}, self.set_cookie(tok))
            if p == "/api/auth/login/begin":
                return self.j(200, auth.login_begin(self.rp_id()))
            if p == "/api/auth/login/finish":
                cred = auth.assert_finish(str(b.get("ticket", "")), str(b.get("id", "")), b.get("response") or {}, "login", ip)
                tok = auth.new_session(cred["id"], False, ua, ip, cred["name"])
                return self.j(200, {"ok": True}, self.set_cookie(tok))
            sess = self.authed()
            if p == "/api/auth/logout":
                auth.logout(self.cookie())
                return self.j(200, {"ok": True}, self.set_cookie("", 0))
            if not sess:
                raise AuthError(401, "unauthorized")
            key, rec = sess
            if p == "/api/auth/stepup/begin":
                if rec["weak"]:
                    raise AuthError(403, "a plain-HTTP session cannot do step-up; use a passkey session")
                return self.j(200, auth.login_begin(self.rp_id(), "stepup", rec["cred"]))
            if p == "/api/auth/stepup/finish":
                auth.stepup(key, str(b.get("ticket", "")), str(b.get("id", "")), b.get("response") or {}, ip)
                return self.j(200, {"ok": True})
            raise AuthError(404, "not found")

        # ---- hub API ----
        def api(self, m, p, url, key, rec):
            if m == "GET" and p == "/api/me":
                return self.j(200, {"name": rec["name"], "weak": rec["weak"], "local": is_local(self.peer, self.hdrs()), "fresh": auth.is_fresh(rec, is_local(self.peer, self.hdrs())),
                                    "devices": auth.devices(rec["cred"]), "sessions": auth.sessions(key), "push_key": store.d["vapid"]["pub"],
                                    "push_subs": len(store.d["push"]), "rp": self.rp_id(), "version": 1})
            if m == "GET" and p == "/api/rooms":
                return self.j(200, {"rooms": hub.digests(), "attention": hub.attention()})
            if m == "GET" and p == "/api/attention":
                return self.j(200, {"items": hub.attention()})
            if m == "GET" and p == "/api/events":
                return self.events()
            if m == "POST" and p == "/api/rooms":
                b = self.body()
                try:
                    room = hub.add(str(b.get("label", "")), str(b.get("machine", "")), str(b.get("kind", "shared")), b.get("home") or None)
                except ValueError as e:
                    raise AuthError(400, str(e))
                link = str(b.get("link", "")).strip()
                if link:
                    sc = room.ensure()
                    if not sc:
                        raise AuthError(502, room.err or "sidecar unavailable")
                    err, data = sc.call("playbox_join", {"link": link})
                    if err:
                        hub.remove(room.id)
                        raise AuthError(400, data if isinstance(data, str) else json.dumps(data))
                    hub.wake.set()
                return self.j(200, {"room": room.digest, "id": room.id})
            if p.startswith("/api/rooms/"):
                rid = p.split("/")[3]
                if m == "PATCH":
                    b = self.body()
                    room = hub.update(rid, **{k: b.get(k) for k in ("label", "machine", "kind")})
                    return self.j(200 if room else 404, {"room": room.digest} if room else {"error": "no such room"})
                if m == "DELETE":
                    if not self.need_fresh(rec):
                        return
                    return self.j(200 if hub.remove(rid) else 404, {"ok": True})
            if m == "POST" and p == "/api/devices/pair":
                if rec["weak"] and not is_local(self.peer, self.hdrs()):
                    raise AuthError(403, "only a passkey session (or the hub machine itself) can pair new devices")
                if not self.need_fresh(rec):
                    return
                code = auth.mint_code(note="added from " + rec["name"])
                return self.j(200, {"code": code, "expires_in": 600})
            if m == "DELETE" and p.startswith("/api/devices/"):
                if not self.need_fresh(rec):
                    return
                cid = urllib.parse.unquote(p.split("/", 3)[3])
                ok = auth.revoke_device(cid)
                return self.j(200 if ok else 404, {"ok": ok})
            if m == "DELETE" and p.startswith("/api/sessions/"):
                ok = auth.revoke_session(p.split("/", 3)[3])
                return self.j(200 if ok else 404, {"ok": ok})
            if m == "POST" and p == "/api/push/subscribe":
                ep = str(self.body().get("endpoint", ""))
                if not ep.startswith("https://") or len(ep) > 1000:
                    raise AuthError(400, "bad endpoint")
                with store.lock:
                    if not any(s["endpoint"] == ep for s in store.d["push"]):
                        store.d["push"].append({"endpoint": ep, "name": rec["name"], "created": time.time()})
                        store.save()
                return self.j(200, {"ok": True})
            if m == "POST" and p == "/api/push/unsubscribe":
                ep = str(self.body().get("endpoint", ""))
                with store.lock:
                    store.d["push"] = [s for s in store.d["push"] if s["endpoint"] != ep]
                    store.save()
                return self.j(200, {"ok": True})
            raise AuthError(404, "not found")

        def events(self):
            q = hub.bus.subscribe()
            self.send_response(200)
            for k, v in (("Content-Type", "text/event-stream"), ("Cache-Control", "no-store"), ("X-Accel-Buffering", "no"),
                         ("Connection", "keep-alive"), ("Content-Security-Policy", CSP)):
                self.send_header(k, v)
            self.end_headers()
            self.close_connection = True
            try:
                hub.wake.set()
                self.wfile.write(b"retry: 3000\n\n")
                self.wfile.write(sse("hello", {"rooms": hub.digests(), "attention": hub.attention()}))
                self.wfile.flush()
                last = time.time()
                while True:
                    try:
                        kind, data = q.get(timeout=15)
                        self.wfile.write(sse(kind, data))
                    except queue.Empty:
                        self.wfile.write(b": ping\n\n")
                    self.wfile.flush()
                    if not auth.session(self.cookie()):
                        self.wfile.write(sse("bye", {}))
                        self.wfile.flush()
                        return
            except (BrokenPipeError, ConnectionResetError, OSError):
                pass
            finally:
                hub.bus.unsubscribe(q)

        # ---- per-room proxy ----
        def room_route(self, m, p, key, rec):
            parts = p.split("/", 3)       # ['', 'r', id, rest]
            room = hub.get(parts[2]) if len(parts) > 2 else None
            if not room:
                return self.j(404, {"error": "no such room"})
            rest = "/" + (parts[3] if len(parts) > 3 else "")
            if m == "GET" and rest == "/":
                return self.send(200, (STATIC / "index.html").read_bytes(), "text/html; charset=utf-8")
            if m == "GET" and rest == "/api/state":
                sc = room.ensure()
                if not sc:
                    return self.j(200, {"ok": False, "status": None, "tools": [], "error": room.err})
                err, st = sc.call("playbox_status", {}, wait=2)
                return self.j(200, {"ok": not err, "status": st, "tools": sc.tools})
            if m == "GET" and rest == "/api/witness":
                from playbox_ui import witness_state
                return self.j(200, witness_state(room.home))
            if m == "POST" and rest == "/api/tool":
                b = self.body()
                name, args = b.get("name"), b.get("args") or {}
                sc = room.ensure()
                if not sc:
                    return self.j(502, {"error": True, "data": room.err})
                if name not in sc.tools:
                    return self.j(400, {"error": f"unknown tool {name}"})
                if name in SENSITIVE_TOOLS and not self.need_fresh(rec):
                    return
                err, data = sc.call(name, args, wait=3 if b.get("poll") else None)
                if name == "playbox_feed" and not err and isinstance(data, dict) and data.get("items"):
                    room.seen()
                    hub.bus.publish("room", {**room.digest, "unread": 0})
                if name in ("playbox_create", "playbox_join", "playbox_leave", "playbox_revoke"):
                    hub.wake.set()
                return self.j(200, {"error": err, "data": data})
            if m == "POST" and rest == "/api/tamper":
                return self.tamper(room)
            return self.j(404, {"error": "not found"})

        def tamper(self, room):
            b = self.body()
            home = Path(room.home).resolve()
            try:
                src = Path(str(b["path"])).expanduser().resolve()
            except (KeyError, OSError):
                raise AuthError(400, "need {path}")
            if not (src.is_file() and str(src).startswith(str(home) + os.sep)):
                raise AuthError(400, "only logs under the room's Playbox home can be used")
            data = bytearray(src.read_bytes())
            if len(data) < 2:
                raise AuthError(400, "log is empty")
            off = len(data) // 2
            data[off] ^= 1
            tmp = home / "ui-demo"
            tmp.mkdir(parents=True, exist_ok=True, mode=0o700)
            copy = tmp / "tampered.log"
            copy.write_bytes(bytes(data))
            try:
                sc = room.ensure()
                err, res = sc.call("playbox_verify", {"path": str(copy)})
            finally:
                copy.unlink(missing_ok=True)
            self.j(200, {"offset": off, "bytes": len(data), "entries": bytes(data).count(b"\n"), "error": err, "result": res})

    return H


def sse(kind, data):
    return f"event: {kind}\ndata: {json.dumps(data, separators=(',', ':'))}\n\n".encode()


MANIFEST = {
    "name": "Playbox", "short_name": "Playbox", "description": "Every Playbox room, yours and shared, in one place.",
    "id": "/", "start_url": "/", "scope": "/", "display": "standalone", "orientation": "any",
    "background_color": "#12122a", "theme_color": "#12122a", "categories": ["developer", "productivity"],
    "icons": [{"src": "/icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any"},
              {"src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any"},
              {"src": "/icon-maskable.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable"},
              {"src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any"}],
    "shortcuts": [{"name": "Attention", "url": "/?view=attention"}]}

ICON_SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512"><rect width="512" height="512" rx="112" fill="#12122a"/>
<circle cx="256" cy="256" r="150" fill="none" stroke="#e8c872" stroke-width="14" opacity=".35"/><circle cx="256" cy="256" r="92" fill="none" stroke="#e8c872" stroke-width="14" opacity=".65"/>
<circle cx="256" cy="256" r="34" fill="#e8c872"/><circle cx="406" cy="256" r="16" fill="#aab4ff"/><circle cx="174" cy="126" r="12" fill="#7fd6b0"/></svg>"""


def lan_ip():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("10.255.255.255", 1))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()


def tailscale_info():
    ts = shutil.which("tailscale") or ("/Applications/Tailscale.app/Contents/MacOS/Tailscale" if os.path.exists("/Applications/Tailscale.app/Contents/MacOS/Tailscale") else None)
    if not ts:
        return None, None
    try:
        out = subprocess.run([ts, "status", "--json"], capture_output=True, text=True, timeout=8)
        st = json.loads(out.stdout)
        if st.get("BackendState") != "Running":
            return ts, None
        return ts, (st.get("Self", {}).get("DNSName") or "").rstrip(".") or None
    except (OSError, subprocess.SubprocessError, ValueError):
        return ts, None


def main(argv=None):
    ap = argparse.ArgumentParser(prog="playbox_hub", description="Playbox hub: every room, one installable app.")
    ap.add_argument("--port", type=int, default=7717)
    ap.add_argument("--home", default=os.environ.get("PLAYBOX_HUB_HOME", str(Path.home() / ".playbox-hub")))
    ap.add_argument("--lan", action="store_true", help="also listen on the local network (phones on your Wi-Fi can pair over plain HTTP)")
    ap.add_argument("--tailscale", action="store_true", help="publish to your tailnet over HTTPS with `tailscale serve` (private to your devices; passkeys work there)")
    ap.add_argument("--require-tailnet-login", metavar="LOGIN", help="with --tailscale: also require this Tailscale user (header set by tailscaled)")
    ap.add_argument("--no-weak", action="store_true", help="refuse plain-HTTP pairing entirely: passkeys only")
    ap.add_argument("--pair", action="store_true", help="print a fresh pairing link for a new device")
    ap.add_argument("--mcp", default=os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp") or shutil.which("playbox"))
    a = ap.parse_args(argv)
    if not a.mcp:
        sys.exit("playbox not found. Install it: curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh")
    store = Store(a.home)
    auth = Auth(store, allow_weak=not a.no_weak)

    def push_fn(items):
        v = store.d["vapid"]
        dead = []
        for sub in list(store.d["push"]):
            if webpush.send(int(v["d"], 16), v["pub"], sub["endpoint"]) in (404, 410):
                dead.append(sub["endpoint"])
        if dead:
            with store.lock:
                store.d["push"] = [s for s in store.d["push"] if s["endpoint"] not in dead]
                store.save()

    hub = Hub(store, a.mcp, push_fn)
    opts = {"weak": not a.no_weak, "tailnet_login": a.require_tailnet_login}
    host = "0.0.0.0" if a.lan else "127.0.0.1"
    srv = http.server.ThreadingHTTPServer((host, a.port), make_handler(hub, auth, store, opts))
    srv.daemon_threads = True
    port = srv.server_address[1]
    base = f"http://127.0.0.1:{port}"
    ts_name = None
    if a.tailscale:
        ts, ts_name = tailscale_info()
        if not ts_name:
            print("Tailscale is not running or not installed; skipping --tailscale.", flush=True)
        else:
            r = subprocess.run([ts, "serve", "--bg", f"--https=443", f"http://127.0.0.1:{port}"], capture_output=True, text=True, timeout=30)
            if r.returncode != 0:
                print("`tailscale serve` failed: " + (r.stderr or r.stdout).strip()[:200], flush=True)
                ts_name = None
    print(f"\n  ☤  Playbox hub   {base}", flush=True)
    if not auth.enrolled() or a.pair:
        code = auth.mint_code()
        print(f"\n  Pair this device (valid 10 min, single use):\n    {base}/pair#{code}", flush=True)
        if ts_name:
            print(f"    https://{ts_name}/pair#{code}      <- passkeys, from any of your devices", flush=True)
        if a.lan and opts["weak"]:
            print(f"    http://{lan_ip()}:{port}/pair#{code}   <- phone on this Wi-Fi (plain HTTP)", flush=True)
    else:
        print(f"  Sign in with your passkey at {base}" + (f" or https://{ts_name}" if ts_name else ""), flush=True)
        print("  New device? restart with --pair, or use Devices inside the app.", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        hub.close()
