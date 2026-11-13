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
from .access import Access, AccessError
from .auth import Auth, AuthError, PROXY_HEADERS, hostname_of
from . import agents as agentmod
from .icons import png_icon
from .invites import Invites, InviteError
from .knock import KnockError, Knocks, Outgoing, LINK_TTL
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
    invites = Invites(store)

    class H(http.server.BaseHTTPRequestHandler):
        server_version = "playbox-hub"
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        # ---- plumbing ----
        @property
        def peer(self):
            ip = self.client_address[0]
            # behind our own cloudflared (loopback peer only) the real client is the header Cloudflare sets, so the throttle is per visitor, not global
            if opts.get("cloudflare") and ip in ("127.0.0.1", "::1"):
                return self.headers.get("Cf-Connecting-Ip", ip)
            return ip

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

        def access_ok(self):
            """With Cloudflare Access configured, anything that did not arrive straight on loopback must carry a valid Access token."""
            acc = opts.get("access")
            if not acc or is_local(self.client_address[0], self.hdrs()):
                return True
            tok = self.headers.get("Cf-Access-Jwt-Assertion", "")
            try:
                acc.verify(tok)
                return True
            except AccessError:
                self.send(403, b"This hub only accepts requests that passed Cloudflare Access.", "text/plain; charset=utf-8")
                return False

        @staticmethod
        def invite_public(m, p):
            """The only paths a friend's browser may reach without Cloudflare Access: the invite page, its status/knock/poll calls
            (each needs the invite's own secret or is a read-only status) and the icon. Everything else needs Access."""
            if m == "GET" and (p == "/icon.svg" or (p.startswith("/i/") and len(p) > 3 and "/" not in p[3:])):
                return True
            parts = p.split("/")      # ['', 'api', 'invite', id, action?]
            if parts[:3] == ["", "api", "invite"] and len(parts) in (4, 5) and parts[3]:
                return (m == "GET" and len(parts) == 4) or (m == "POST" and len(parts) == 5 and parts[4] in ("knock", "poll"))
            return False

        def route(self, m):
            url = urllib.parse.urlparse(self.path)
            p = url.path
            if not self.invite_public(m, p) and not self.access_ok():
                return
            # public shell
            if m == "GET":
                if p in ("/", "/pair", "/join"):
                    return self.send(200, (STATIC / "home.html").read_bytes(), "text/html; charset=utf-8")
                if p.startswith("/i/") and len(p) > 3:
                    return self.send(200, (STATIC / "invite.html").read_bytes(), "text/html; charset=utf-8")
                if p.startswith("/api/invite/"):
                    auth.throttle(self.peer)
                    iid = p.rsplit("/", 1)[1]
                    rec0 = store.d["invites"].get(iid)
                    room0 = hub.get(rec0["room"]) if rec0 else None
                    v = invites.public(iid, room0.digest.get("members") if room0 else None)
                    if not v:
                        auth._fail(self.peer)       # ids are 128-bit; a stream of misses is someone guessing
                    return self.j(200 if v else 404, v or {"state": "unknown"})
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
            if p.startswith("/api/invite/") and m == "POST":
                return self.invite_post(p)
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

        def invite_post(self, p):
            """Public, for people holding an approval invite: knock, then poll for the answer. Authenticated by the invite's secret `k`."""
            auth.throttle(self.peer)
            parts = p.split("/")      # ['', 'api', 'invite', id, action]
            rec0 = store.d["invites"].get(parts[3]) if len(parts) == 5 else None
            if not rec0 or parts[4] not in ("knock", "poll"):
                auth._fail(self.peer)
                raise AuthError(404, "no such invite")
            b = self.body()
            try:
                if parts[4] == "knock":
                    r = hub.knocks.knock(rec0, str(b.get("k", "")), str(b.get("guest", "")), str(b.get("name", "")))
                    hub.wake.set()
                    return self.j(200, {"rid": r["rid"], "code": r["code"]})
                return self.j(200, hub.knocks.poll(rec0, str(b.get("k", "")), str(b.get("rid", ""))))
            except KnockError as e:
                if e.code in (403, 404):
                    auth._fail(self.peer)
                raise AuthError(e.code, str(e))

        def approve_knock(self, rid, rec, deny=False):
            if not deny and not self.need_fresh(rec):      # letting someone in: a passkey touch
                return
            r = hub.knocks.get(rid)
            if not r or r["state"] != "pending":
                raise AuthError(404, "that request is gone")
            if deny:
                hub.knocks.set(rid, state="denied")
                hub.wake.set()
                return self.j(200, {"ok": True})
            inv = store.d["invites"].get(r["invite"])
            room = hub.get(r["room"])
            if not inv or not room or invites.state(inv) not in ("pending",):
                raise AuthError(410, "that invite is no longer open")
            if len([x for x in store.d["knocks"].values() if x["invite"] == inv["id"] and x["state"] in ("approved", "done")]) >= inv["uses"]:
                raise AuthError(410, "this invite has already let in everyone it was for")
            sc = room.ensure()
            err, st = sc.call("playbox_status", {}) if sc else (True, None)
            host = (st or {}).get("hosting") if isinstance(st, dict) else None
            if err or not host or host.get("replica"):
                raise AuthError(400, "this room isn't hosted from this hub any more")
            err, data = sc.call("playbox_create", {**({"repo": host["repo"]} if host.get("repo") else {}), "invite_ttl_secs": LINK_TTL, "grant_ttl_secs": inv["grant"], "uses": 1, "for_guest": r["guest"]})
            if err or not isinstance(data, dict) or not data.get("link"):
                raise AuthError(502, data if isinstance(data, str) else "the room did not return a link")
            hub.knocks.set(rid, state="approved", link=data["link"])
            hub.wake.set()
            return self.j(200, {"ok": True})

        def public_base(self):
            """The address a friend can open. The one this request came in on if that is public, else the tunnel the hub published."""
            host = self.headers.get("Host", "")
            h = hostname_of(host)
            private = h in ("localhost",) or h.endswith(".local")
            try:
                a = ipaddress.ip_address(h)
                private = private or a.is_private or a.is_loopback or a.is_link_local
            except ValueError:
                pass
            if not private:
                return f"{'https' if self.secure() else 'http'}://{host}", True
            if opts.get("public_url"):
                return opts["public_url"], True
            return f"{'https' if self.secure() else 'http'}://{host}", False

        def make_invite(self, rid, rec):
            if not self.need_fresh(rec):      # an invite is access to a room: ask for a passkey touch
                return
            b = self.body()
            room = hub.get(rid)
            if not room:
                raise AuthError(404, "no such room")
            sc = room.ensure()
            if not sc:
                raise AuthError(502, room.err or "the room's sidecar is unavailable")
            err, st = sc.call("playbox_status", {})
            host = (st or {}).get("hosting") if isinstance(st, dict) else None
            if err or not host or host.get("replica"):
                raise AuthError(400, "This room isn't hosted from this hub, so it can't mint invites. Open it and host it first, or ask the room's host.")
            try:
                ttl, grant, uses = int(b.get("ttl", 3600)), int(b.get("grant", 86400)), int(b.get("uses", 1))
                guest = str(b["guest"]).strip().lower() if b.get("guest") else None
                approve = bool(b.get("approve", True))
                inv, k = invites.create(rid, room.rec["label"], str(b.get("from", "")).strip(), ttl, grant, uses, len(host.get("members") or []), guest, approve)
            except (ValueError, TypeError) as e:
                raise AuthError(400, str(e))
            if approve:       # no key exists yet: the room mints one, bound to the asker, when you approve
                base, public = self.public_base()
                return self.j(200, {"id": inv["id"], "url": f"{base}/i/{inv['id']}", "k": k, "approve": True, "expires": int(inv["expires"]), "public": public})
            args = {**({"repo": host["repo"]} if host.get("repo") else {}), "invite_ttl_secs": ttl, "grant_ttl_secs": grant, "uses": uses}
            if guest:
                args["for_guest"] = guest
            err, data = sc.call("playbox_create", args)
            if err or not isinstance(data, dict) or not data.get("link"):
                invites.revoke(inv["id"])
                raise AuthError(502, data if isinstance(data, str) else "the room did not return a link")
            base, public = self.public_base()
            return self.j(200, {"id": inv["id"], "url": f"{base}/i/{inv['id']}", "link": data["link"], "expires": int(inv["expires"]), "public": public})

        # ---- hub API ----
        def api(self, m, p, url, key, rec):
            if m == "GET" and p == "/api/me":
                return self.j(200, {"name": rec["name"], "weak": rec["weak"], "local": is_local(self.peer, self.hdrs()), "fresh": auth.is_fresh(rec, is_local(self.peer, self.hdrs())),
                                    "devices": auth.devices(rec["cred"]), "sessions": auth.sessions(key), "push_key": store.d["vapid"]["pub"],
                                    "push_subs": len(store.d["push"]), "rp": self.rp_id(), "version": 1})
            if m == "GET" and p == "/api/rooms":
                return self.j(200, {"rooms": hub.digests(), "attention": hub.attention(), "activity": hub.recent()})
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
            if m == "GET" and p == "/api/agents":
                return self.j(200, {"agents": agentmod.detect()})
            if m == "POST" and p.startswith("/api/agents/") and p.endswith("/connect"):
                if not self.need_fresh(rec):      # this edits the agent's own config files
                    return
                ok, out = agentmod.connect(p.split("/")[3])
                return self.j(200 if ok else 502, {"ok": ok, "output": out, "agents": agentmod.detect()})
            if m == "GET" and p == "/api/knocks":
                return self.j(200, {"requests": [{"rid": r["rid"], "name": r["name"], "code": r["code"], "room": r["room"], "created": r["created"],
                                                  "label": store.d["invites"].get(r["invite"], {}).get("label", "")} for r in hub.knocks.pending()]})
            if m == "POST" and p.startswith("/api/knocks/") and p.rsplit("/", 1)[1] in ("approve", "deny"):
                return self.approve_knock(p.split("/")[3], rec, deny=p.endswith("/deny"))
            if m == "POST" and p == "/api/knock":
                b = self.body()
                if hub.outgoing_busy():
                    raise AuthError(429, "you already have requests waiting; let them finish first")
                try:
                    r = hub.outgoing.start(str(b.get("base", "")), str(b.get("id", "")), str(b.get("k", "")), str(b.get("label", ""))[:60],
                                           str(b.get("name", "")).strip()[:40] or rec["name"][:40], b.get("agent_guest") or None)
                except KnockError as e:
                    raise AuthError(e.code if 400 <= e.code < 600 else 502, str(e))
                return self.j(200, {"rid": r["rid"], "code": r["code"], "room": r["room"]})
            if m == "GET" and p == "/api/knock/out":
                return self.j(200, {"requests": hub.outgoing.listing()})
            if m == "DELETE" and p.startswith("/api/knock/out/"):
                rid2 = p.rsplit("/", 1)[1]
                return self.j(200, {"ok": hub.outgoing.dismiss(rid2)})
            if m == "GET" and p == "/api/invites":
                return self.j(200, {"invites": invites.listing(hub.digests())})
            if m == "DELETE" and p.startswith("/api/invites/"):
                return self.j(200, {"ok": invites.revoke(p.rsplit("/", 1)[1])})
            if m == "POST" and p.startswith("/api/rooms/") and p.endswith("/invite"):
                return self.make_invite(p.split("/")[3], rec)
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
            for k, v in (("Content-Type", "text/event-stream"), ("Cache-Control", "no-cache, no-transform"), ("X-Accel-Buffering", "no"),
                         ("Transfer-Encoding", "chunked"), ("Content-Security-Policy", CSP)):
                self.send_header(k, v)
            self.end_headers()
            self.close_connection = True
            def emit(b):    # proper chunked framing: proxies and tunnels flush each event instead of waiting for the stream to end
                self.wfile.write(b"%x\r\n" % len(b) + b + b"\r\n")
            try:
                hub.wake.set()
                emit(b"retry: 3000\n\n")
                emit(sse("hello", {"rooms": hub.digests(), "attention": hub.attention(), "activity": hub.recent()}))
                self.wfile.flush()
                last = time.time()
                while True:
                    try:
                        kind, data = q.get(timeout=15)
                        emit(sse(kind, data))
                    except queue.Empty:
                        emit(b": ping\n\n")
                    self.wfile.flush()
                    if not auth.session(self.cookie()):
                        emit(sse("bye", {}))
                        self.wfile.write(b"0\r\n\r\n")
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
                if name == "playbox_create" and not err and not args.get("replica_of"):
                    keep = {k: args[k] for k in ("repo", "local", "docker") if args.get(k)}       # what a restart must host again
                    if room.rec.get("host") != keep:
                        room.rec["host"] = keep
                        room.on_change and room.on_change()
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
    "background_color": "#171715", "theme_color": "#171715", "categories": ["developer", "productivity"],
    "icons": [{"src": "/icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any"},
              {"src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any"},
              {"src": "/icon-maskable.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable"},
              {"src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any"}],
    "shortcuts": [{"name": "Attention", "url": "/?view=attention"}],
    "protocol_handlers": [{"protocol": "web+playbox", "url": "/join#%s"}]}

ICON_SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512"><rect width="512" height="512" rx="96" fill="#171715"/><path d="M340 80h40v40h-40zM184 132h40v40h-40zM236 132h40v40h-40zM288 132h40v40h-40zM184 184h40v40h-40zM236 184h40v40h-40zM288 184h40v40h-40zM340 236h40v40h-40zM184 288h40v40h-40zM236 288h40v40h-40zM288 288h40v40h-40zM340 288h40v40h-40zM184 340h40v40h-40zM236 340h40v40h-40zM288 340h40v40h-40zM340 340h40v40h-40zM184 392h40v40h-40zM236 392h40v40h-40zM288 392h40v40h-40zM340 392h40v40h-40z" fill="#2A2A26"/><path d="M132 80h40v40h-40zM184 80h40v40h-40zM236 80h40v40h-40zM288 80h40v40h-40zM132 132h40v40h-40zM340 132h40v40h-40zM132 184h40v40h-40zM340 184h40v40h-40zM132 236h40v40h-40zM184 236h40v40h-40zM236 236h40v40h-40zM288 236h40v40h-40zM132 288h40v40h-40zM132 340h40v40h-40zM132 392h40v40h-40z" fill="#FF5A1F"/></svg>"""


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


def start_cloudflared(port, tunnel=None, hostname=None):
    """Run cloudflared in front of the hub. Quick tunnel (no account): random https://*.trycloudflare.com. Named tunnel: your own hostname.
    Returns (process, https_url) or (None, None)."""
    import re
    import threading
    cf = shutil.which("cloudflared")
    if not cf:
        print("cloudflared is not installed (brew install cloudflared); skipping --cloudflare.", flush=True)
        return None, None
    cmd = [cf, "tunnel", "--no-autoupdate", "--url", f"http://127.0.0.1:{port}"]
    if tunnel:
        # a tunnel given by id runs from its credentials file, so no account login (cert.pem) is needed at run time
        creds = Path.home() / ".cloudflared" / f"{tunnel}.json"
        cmd += (["--credentials-file", str(creds)] if creds.exists() else []) + ["run", tunnel]
    proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    found = threading.Event()
    box = {}

    def pump():
        for line in proc.stderr:      # keep draining so cloudflared never blocks on a full pipe
            m = re.search(r"https://[a-z0-9-]+\.trycloudflare\.com", line)
            if m and "url" not in box:
                box["url"] = m.group(0)
                found.set()
            if tunnel and "Registered tunnel connection" in line:
                box.setdefault("url", f"https://{hostname}")
                found.set()
    threading.Thread(target=pump, daemon=True).start()
    if not found.wait(40) or proc.poll() is not None:
        print("cloudflared did not come up within 40 s; skipping --cloudflare.", flush=True)
        proc.terminate()
        return None, None
    return proc, box["url"]


def main(argv=None):
    ap = argparse.ArgumentParser(prog="playbox_hub", description="Playbox hub: every room, one installable app.")
    ap.add_argument("--port", type=int, default=int(os.environ.get("PLAYBOX_HUB_PORT", "7717")))
    ap.add_argument("--home", default=os.environ.get("PLAYBOX_HUB_HOME", str(Path.home() / ".playbox-hub")))
    ap.add_argument("--lan", action="store_true", help="also listen on the local network (phones on your Wi-Fi can pair over plain HTTP)")
    ap.add_argument("--tailscale", action="store_true", help="publish to your tailnet over HTTPS with `tailscale serve` (private to your devices; passkeys work there)")
    ap.add_argument("--require-tailnet-login", metavar="LOGIN", help="with --tailscale: also require this Tailscale user (header set by tailscaled)")
    ap.add_argument("--cloudflare", action="store_true", help="publish over HTTPS with a Cloudflare quick tunnel (cloudflared, no account, no open port). Passkeys only; the address changes each run")
    ap.add_argument("--cloudflare-tunnel", metavar="NAME", help="use your own named Cloudflare tunnel (stable address, so passkeys survive restarts); needs --cloudflare-hostname")
    ap.add_argument("--cloudflare-access", metavar="TEAM", help="Cloudflare Access team (name or name.cloudflareaccess.com): refuse any tunnelled request without a valid Access token; needs --cloudflare-aud")
    ap.add_argument("--cloudflare-aud", metavar="AUD", help="the Access application's Audience (AUD) tag")
    ap.add_argument("--cloudflare-hostname", metavar="HOST", help="the public hostname routed to that named tunnel")
    ap.add_argument("--no-weak", action="store_true", help="refuse plain-HTTP pairing entirely: passkeys only")
    ap.add_argument("--join", metavar="LINK", help="join this playbox:// invite as a new room before starting (the installer uses this)")
    ap.add_argument("--room-name", metavar="NAME", help="what to call the room joined with --join")
    ap.add_argument("--open", action="store_true", dest="open_browser", help="open the pairing page (or the app, if already paired) in your browser")
    ap.add_argument("--pair", action="store_true", help="print a fresh pairing link for a new device")
    ap.add_argument("--mcp", default=os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp") or shutil.which("playbox"))
    a = ap.parse_args(argv)
    if not a.mcp:
        sys.exit("playbox not found. Install it: curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh")
    cloud = a.cloudflare or bool(a.cloudflare_tunnel)
    team, aud = a.cloudflare_access or os.environ.get("PLAYBOX_ACCESS_TEAM"), a.cloudflare_aud or os.environ.get("PLAYBOX_ACCESS_AUD")
    if team and not cloud:
        sys.exit("Cloudflare Access only applies with --cloudflare or --cloudflare-tunnel.")
    if bool(team) != bool(aud):
        sys.exit("Cloudflare Access needs both the team (--cloudflare-access) and the AUD tag (--cloudflare-aud).")
    if a.cloudflare_tunnel and not a.cloudflare_hostname:
        sys.exit("--cloudflare-tunnel needs --cloudflare-hostname (the public name routed to that tunnel).")
    if cloud:
        a.no_weak = True          # a public address never gets the plain-HTTP pairing path: passkeys only
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
    hub.knocks = Knocks(store)
    hub.outgoing = Outgoing(hub, store)
    hub.outgoing_busy = lambda: len([r for r in store.d["knock_out"].values() if r["state"] == "waiting"]) >= 3
    hub.extra_attention.append(lambda: [{"id": f"knock:{r['rid']}", "room": r["room"], "kind": "knock", "urgent": True, "rid": r["rid"],
                                         "title": f"{r['name']} wants to join {store.d['invites'].get(r['invite'], {}).get('label', 'a room')}",
                                         "body": f"verify code {r['code']}: approve only if they read you the same one"} for r in hub.knocks.pending()])
    opts = {"public_url": None, "weak": not a.no_weak, "tailnet_login": a.require_tailnet_login, "cloudflare": cloud, "access": Access(team, aud) if team else None}
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
    import signal
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))   # `kill` must also take the tunnel/tailnet publication down
    cf_proc = cf_url = None
    if cloud:
        threading_serve = __import__("threading").Thread(target=srv.serve_forever, daemon=True)
        threading_serve.start()     # cloudflared checks the origin as it connects, so serve first
        cf_proc, cf_url = start_cloudflared(port, a.cloudflare_tunnel, a.cloudflare_hostname)
    if cf_url:
        opts["public_url"] = cf_url      # the address friend invites point at (a tailnet address only reaches your own devices)
    link = a.join or os.environ.get("PLAYBOX_JOIN_LINK")
    if link and link.startswith(("https://", "http://")):          # an ask-to-join invite page: knock, then wait for the host
        try:
            u = urllib.parse.urlparse(link)
            iid, frag = u.path.rstrip("/").rsplit("/", 1)[-1], u.fragment
            if not frag.startswith("a.") or "/i/" not in u.path:
                raise ValueError("expected https://<host>/i/<id>#a.<key>")
            who = os.environ.get("PLAYBOX_JOIN_AS") or os.environ.get("USER") or "New member"
            kr = hub.outgoing.start(f"{u.scheme}://{u.netloc}", iid, frag[2:], a.room_name or os.environ.get("PLAYBOX_JOIN_NAME") or "Invited room", who)
            print(f"\n  Asked to join \"{kr['label']}\".\n  Tell the person who invited you this code, out loud or in a message you trust:\n\n      {kr['code']}\n\n  They approve it on their side and the room appears in your dashboard.", flush=True)
        except (KnockError, ValueError) as e:
            print(f"  Couldn't ask to join: {e}\n  Ask for a fresh invite; the hub will start anyway.", flush=True)
    elif link:
        if not link.startswith("playbox://"):
            print("--join: that doesn't look like a playbox:// invite or an invite page; skipping.", flush=True)
        else:
            try:
                jroom = hub.add(a.room_name or os.environ.get("PLAYBOX_JOIN_NAME") or "Invited room", "this machine", "shared")
                jsc = jroom.ensure()
                jerr, jdata = jsc.call("playbox_join", {"link": link}) if jsc else (True, jroom.err or "sidecar unavailable")
            except ValueError as e:
                jroom, jerr, jdata = None, True, str(e)
            if jerr:
                if jroom:
                    hub.remove(jroom.id)
                print(f"  Couldn't join that invite: {jdata if isinstance(jdata, str) else json.dumps(jdata)[:200]}\n  It may have expired or been used. Ask for a fresh link; the hub will start anyway.", flush=True)
            else:
                hub.wake.set()
                print(f"  Joined \"{jroom.rec['label']}\".", flush=True)
    local = f"http://localhost:{port}"          # passkeys need a real hostname as their relying party, and an IP address isn't one
    print(f"\n  ▪ PLAYBOX hub   {base}", flush=True)
    if cf_url:
        print(f"  Public (Cloudflare, HTTPS, passkeys only): {cf_url}", flush=True)
    if (not auth.enrolled() or a.pair or (cf_url and not a.cloudflare_tunnel)):
        code = auth.mint_code()
        print(f"\n  Pair this device (valid 10 min, single use):\n    {local}/pair#{code}", flush=True)
        open_url = f"{local}/pair#{code}"
        if cf_url:
            print(f"    {cf_url}/pair#{code}      <- passkeys, from any device (a quick tunnel's address changes each run, so pair again)", flush=True)
        if ts_name:
            print(f"    https://{ts_name}/pair#{code}      <- passkeys, from any of your devices", flush=True)
        if a.lan and opts["weak"]:
            print(f"    http://{lan_ip()}:{port}/pair#{code}   <- phone on this Wi-Fi (plain HTTP)", flush=True)
    else:
        open_url = local + "/"
        print(f"  Sign in with your passkey at {local}" + (f" or https://{ts_name}" if ts_name else ""), flush=True)
        print("  New device? restart with --pair, or use Devices inside the app.", flush=True)
    if a.open_browser:
        try:
            import webbrowser
            webbrowser.open(open_url)
        except Exception:
            pass
    try:
        if cloud:
            while threading_serve.is_alive():
                threading_serve.join(1)      # short joins so signals are handled promptly
        else:
            srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        hub.close()
        if cf_proc:
            cf_proc.terminate()
        if ts_name:    # take the tailnet publication down with the hub
            subprocess.run([ts, "serve", "--https=443", "off"], capture_output=True, timeout=15)
