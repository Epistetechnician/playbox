#!/usr/bin/env python3
"""Playbox cockpit: a local web UI for a Playbox room, for desktop and phone.

Runs its own `playbox-mcp` sidecar (stdio) and serves a single-page app plus a small JSON API.
Stdlib only. Binds to 127.0.0.1 unless --lan is given; every request needs the access token.

    python3 ui/playbox_ui.py            # http://127.0.0.1:7717/?t=<token>
    python3 ui/playbox_ui.py --lan      # also reachable from a phone on the same network
"""
import argparse
import hashlib
import hmac
import http.server
import socketserver
import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import sys
import queue
import threading
import time
import urllib.parse
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAX_BODY = 1 << 20


class Sidecar:
    """One `playbox-mcp` process, spoken to over line-delimited JSON-RPC. Calls are serialized."""

    def __init__(self, binary, home):
        env = dict(os.environ)
        if home:
            env["PLAYBOX_HOME"] = home
        self.proc = subprocess.Popen([binary, "mcp"] if os.path.basename(binary) == "playbox" else [binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, text=True, env=env, bufsize=1)
        self.lock = threading.Lock()
        self.next_id = 0
        self.lines = queue.Queue()          # stdout lines, read by a thread so a call can have a deadline
        threading.Thread(target=self._pump, daemon=True, name="sidecar-reader").start()
        self.tools = []
        self._rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                 "clientInfo": {"name": "playbox-ui", "version": "0.1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.tools = [t["name"] for t in self._rpc("tools/list", {}).get("result", {}).get("tools", [])]

    def _pump(self):
        try:
            for line in self.proc.stdout:
                self.lines.put(line)
        finally:
            self.lines.put(None)

    def _send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def _rpc(self, method, params, timeout=None):
        """One request. With `timeout`, stop waiting after that many seconds (raises TimeoutError); the answer, if it ever comes, is skipped by id."""
        self.next_id += 1
        want = self.next_id
        self._send({"jsonrpc": "2.0", "id": want, "method": method, "params": params})
        end = None if timeout is None else time.monotonic() + timeout
        while True:
            try:
                line = self.lines.get(timeout=None if end is None else max(0.0, end - time.monotonic()))
            except queue.Empty:
                raise TimeoutError(f"no answer in {timeout:g} s")
            if line is None:
                self.lines.put(None)
                raise RuntimeError("playbox-mcp exited")
            try:
                msg = json.loads(line)
            except ValueError:
                continue
            if msg.get("id") == want:
                return msg

    def call(self, name, args, wait=None, timeout=None):
        """Returns (is_error, parsed_json_or_text). `wait` seconds to get the lock, else (True, 'busy'). `timeout` seconds for the answer, else (True, 'timeout: ...')
        (a host that is gone can leave a call waiting a very long time; polls and reads must not)."""
        if not self.lock.acquire(timeout=wait if wait is not None else -1):
            return True, "busy"
        try:
            if self.proc.poll() is not None:
                return True, "the playbox-mcp sidecar has exited; restart the UI"
            try:
                res = self._rpc("tools/call", {"name": name, "arguments": args}, timeout).get("result", {})
            except TimeoutError as e:
                return True, f"timeout: {e}"
        finally:
            self.lock.release()
        text = "".join(c.get("text", "") for c in res.get("content", []))
        try:
            return bool(res.get("isError")), json.loads(text)
        except ValueError:
            return bool(res.get("isError")), text

    def close(self):
        try:
            self.proc.terminate()
        except OSError:
            pass


def lan_ip():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("10.255.255.255", 1))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()


def make_handler(sidecar, token, page_path, home):
    class H(http.server.BaseHTTPRequestHandler):
        server_version = "playbox-ui"

        def log_message(self, *a):
            pass

        def _authed(self):
            if hmac.compare_digest(self.headers.get("X-Playbox-Token", ""), token):
                return True
            for part in self.headers.get("Cookie", "").split(";"):
                k, _, v = part.strip().partition("=")
                if k == "pbt" and hmac.compare_digest(v, token):
                    return True
            return False

        def _send(self, code, body, ctype, extra=None):
            data = body if isinstance(body, bytes) else body.encode()
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Referrer-Policy", "no-referrer")
            for k, v in (extra or {}).items():
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(data)

        def _json(self, code, obj):
            self._send(code, json.dumps(obj), "application/json")

        def do_GET(self):
            url = urllib.parse.urlparse(self.path)
            q = urllib.parse.parse_qs(url.query)
            if url.path == "/" and "t" in q:
                if not hmac.compare_digest(q["t"][0], token):
                    return self._send(403, "bad token", "text/plain")
                return self._send(303, "", "text/plain", {
                    "Location": "/",
                    "Set-Cookie": f"pbt={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000"})
            if url.path == "/manifest.webmanifest":
                return self._send(200, json.dumps({
                    "name": "Playbox", "short_name": "Playbox", "start_url": "/", "display": "standalone",
                    "background_color": "#171715", "theme_color": "#171715",
                    "icons": [{"src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any maskable"}]}),
                    "application/manifest+json")
            if url.path == "/icon.svg":
                return self._send(200, ICON, "image/svg+xml")
            if not self._authed():
                return self._send(401, LOCKED, "text/html; charset=utf-8")
            if url.path in ("/instruments.js", "/themes.js", "/qr.js", "/shell.js", "/settings.js", "/markdown.js"):
                return self._send(200, (HERE / "static" / url.path[1:]).read_bytes(), "text/javascript; charset=utf-8")
            if url.path.startswith("/vendor/"):
                ok_path = re.fullmatch(r"/vendor/[A-Za-z0-9._/-]{1,80}", url.path) and ".." not in url.path
                vf = (HERE / "static" / url.path[1:]).resolve() if ok_path else None
                vt = {".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8", ".woff2": "font/woff2"}.get(vf.suffix) if vf else None
                if vt and vf.is_file() and (HERE / "static" / "vendor").resolve() in vf.parents:
                    return self._send(200, vf.read_bytes(), vt)
                return self._json(404, {"error": "not found"})
            if url.path == "/mark.svg":
                return self._send(200, (HERE / "static" / "brand" / "mark.svg").read_bytes(), "image/svg+xml")
            if url.path == "/theme.css":
                return self._send(200, (HERE / "static" / "theme.css").read_bytes(), "text/css; charset=utf-8")
            if url.path == "/":
                return self._send(200, page_path.read_text(), "text/html; charset=utf-8")
            if url.path == "/api/state":
                err, status = sidecar.call("playbox_status", {}, wait=2)
                return self._json(200, {"ok": not err, "status": status, "tools": sidecar.tools})
            if url.path == "/api/witness":
                return self._json(200, witness_state(home))
            if url.path == "/api/repos":
                return self._json(200, {"repos": discover_repos([os.getcwd()])})
            self._send(404, "not found", "text/plain")

        def do_POST(self):
            if not self._authed():
                return self._json(401, {"error": "unauthorized"})
            if self.path == "/api/tamper":
                return self._tamper()
            if self.path == "/api/witness/decide":
                if "application/json" not in self.headers.get("Content-Type", ""):
                    return self._json(415, {"error": "json only"})
                try:
                    n = int(self.headers.get("Content-Length", "0"))
                    if n > MAX_BODY:
                        return self._json(413, {"error": "too large"})
                    req = json.loads(self.rfile.read(n) or b"{}")
                    if not isinstance(req, dict):
                        raise ValueError("bad witness decision")
                    out = witness_decide(home, req.get("session"), req.get("decision"))
                except ValueError as e:
                    return self._json(400, {"error": str(e)})
                return self._json(200, out)
            if self.path != "/api/tool":
                return self._json(404, {"error": "not found"})
            if "application/json" not in self.headers.get("Content-Type", ""):
                return self._json(415, {"error": "json only"})
            try:
                n = int(self.headers.get("Content-Length", "0"))
                if n > MAX_BODY:
                    return self._json(413, {"error": "too large"})
                req = json.loads(self.rfile.read(n) or b"{}")
                name, args = req["name"], req.get("args") or {}
            except (ValueError, KeyError, TypeError):
                return self._json(400, {"error": "need {name, args}"})
            if name not in sidecar.tools:
                return self._json(400, {"error": f"unknown tool {name}"})
            err, data = sidecar.call(name, args, wait=3 if req.get("poll") else None)
            self._json(200, {"error": err, "data": data})

        def _tamper(self):
            """Demo: flip one byte in a COPY of a history log and verify the copy. The real log is never touched."""
            try:
                n = int(self.headers.get("Content-Length", "0"))
                src = Path(json.loads(self.rfile.read(n) or b"{}")["path"]).expanduser().resolve()
            except (ValueError, KeyError, TypeError, OSError):
                return self._json(400, {"error": "need {path}"})
            if not (src.is_file() and str(src).startswith(str(Path(home).resolve()) + os.sep)):
                return self._json(400, {"error": "only logs under the Playbox home can be used"})
            data = bytearray(src.read_bytes())
            entries = bytes(data).count(b"\n")
            if len(data) < 2:
                return self._json(400, {"error": "log is empty"})
            # Change one hex digit to another, so the log still parses and verification reports the broken entry. (Flipping a bit
            # of whatever byte sits in the middle can land on a space or newline and make the log unparseable instead, which
            # `playbox_verify` reports as an error string, not a verdict; that depended on the length of the paths in the log.)
            hexd = b"0123456789abcdef"
            offset = next((i for i in range(len(data) // 2, len(data)) if data[i] in hexd), None)
            if offset is None:
                return self._json(400, {"error": "no hex digit to change in this log"})
            data[offset] = hexd[(hexd.index(data[offset]) + 1) % 16]
            tmp = Path(home) / "ui-demo"
            tmp.mkdir(parents=True, exist_ok=True, mode=0o700)
            copy = tmp / "tampered.log"
            copy.write_bytes(bytes(data))
            try:
                err, res = sidecar.call("playbox_verify", {"path": str(copy)})
            finally:
                copy.unlink(missing_ok=True)
            self._json(200, {"offset": offset, "bytes": len(data), "entries": entries, "error": err, "result": res})

    return H


def witness_state(home):
    """Holds, releases and denials the Hermes witness plugin or owner wrote, newest last. Read-only, never touches the sidecar."""
    path = Path(home) / "witness" / "holds.jsonl"
    events = []
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - 65536))
            for line in f.read().decode("utf-8", "replace").splitlines()[-200:]:
                try:
                    e = json.loads(line)
                    if isinstance(e, dict) and e.get("kind") in ("held", "released", "denied"):
                        events.append({k: e.get(k) for k in ("ts", "kind", "session", "tool", "origin", "by")})
                except ValueError:
                    pass
    except OSError:
        pass
    last = {}
    for e in events:
        last[e["session"]] = e
    active = [e for e in last.values() if e["kind"] == "held"]
    return {"events": events[-50:], "active": active}


def witness_decide(home, session_id, decision):
    """Record a single-use owner decision (`release` or `deny`) for an active witness hold in `home`."""
    if not (isinstance(session_id, str) and re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", session_id) and decision in ("release", "deny")):
        raise ValueError("bad witness decision")
    st = witness_state(home)
    hit = next((e for e in st["active"] if e.get("session") == session_id), None)
    if not hit:
        return {"ok": False, "error": "that hold is already resolved"}
    now_ms = int(time.time() * 1000)
    wdir = Path(home) / "witness"
    ddir = wdir / "decisions"
    wdir.mkdir(parents=True, exist_ok=True, mode=0o700)
    ddir.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        os.chmod(wdir, 0o700)
        os.chmod(ddir, 0o700)
    except OSError:
        pass
    digest_hex = hashlib.sha256(session_id.encode()).hexdigest()
    target = ddir / f"{digest_hex}.json"
    tmp = ddir / f".{digest_hex}.{secrets.token_hex(4)}.tmp"
    payload = json.dumps({"session": session_id, "decision": decision, "ts": now_ms}) + "\n"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        if hasattr(os, "fchmod"):
            os.fchmod(fd, 0o600)
        os.write(fd, payload.encode("utf-8"))
    finally:
        os.close(fd)
    os.replace(tmp, target)
    line = json.dumps({"ts": now_ms, "kind": "released" if decision == "release" else "denied",
                       "session": session_id, "tool": hit.get("tool"), "origin": hit.get("origin"), "by": "owner"}) + "\n"
    hfd = os.open(wdir / "holds.jsonl", os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        if hasattr(os, "fchmod"):
            os.fchmod(hfd, 0o600)
        os.write(hfd, line.encode("utf-8"))
    finally:
        os.close(hfd)
    return {"ok": True, "decision": decision, "session": session_id, **witness_state(home)}


_REPO_CACHE = {}
_REPO_LOCK = threading.Lock()
_SKIP_REPO_DIRS = {"node_modules", "target", ".venv"}
_DEFAULT_REPO_DIRS = ("Documents/GitHub", "GitHub", "code", "Code", "src", "projects", "Projects", "dev", "work")


def discover_repos(extra=()):
    """Discover up to 30 local git repositories sorted by recent `.git` mtime descending, cached for 15 s."""
    roots_env = os.environ.get("PLAYBOX_REPO_ROOTS")
    norm_extra = tuple(str(p) for p in (extra or ()) if p)
    cache_key = (roots_env, norm_extra)
    now = time.monotonic()
    with _REPO_LOCK:
        hit = _REPO_CACHE.get(cache_key)
        if hit and now - hit[0] < 15.0:
            return [dict(x) for x in hit[1]]
    seen = {}

    def add_candidate(path_obj):
        try:
            rp = Path(path_obj).expanduser().resolve()
            if str(rp) in seen or not rp.is_dir():
                return False
            st = (rp / ".git").stat()
            seen[str(rp)] = (st.st_mtime, {"path": str(rp), "name": rp.name})
            return True
        except OSError:
            return False

    for p in norm_extra:
        add_candidate(p)

    if roots_env is not None:
        roots = [Path(p).expanduser() for p in roots_env.split(":") if p.strip()]
    else:
        home = Path.home()
        roots = [home / sub for sub in _DEFAULT_REPO_DIRS]

    def scan_dir(d, depth):
        try:
            with os.scandir(d) as it:
                entries = list(it)
        except OSError:
            return
        for e in entries:
            if e.name.startswith(".") or e.name in _SKIP_REPO_DIRS:
                continue
            try:
                if not e.is_dir(follow_symlinks=False):
                    continue
            except OSError:
                continue
            if add_candidate(e.path):
                continue
            if depth < 2:
                scan_dir(e.path, depth + 1)

    for r in roots:
        if not add_candidate(r):
            scan_dir(r, 1)

    ordered = [item for _, item in sorted(seen.values(), key=lambda x: (-x[0], x[1]["path"]))][:30]
    with _REPO_LOCK:
        _REPO_CACHE[cache_key] = (now, ordered)
    return [dict(x) for x in ordered]


LOCKED = """<!doctype html><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1"><meta name=color-scheme content="light dark">
<body style="background:#0f0f0e;color:#ece9e0;font:15px/1.5 'Helvetica Neue',Helvetica,Arial,sans-serif;display:grid;place-items:center;height:100vh;margin:0">
<div style="max-width:28rem;padding:1.5rem;border:1px solid #6c6a5f;background:#171715"><div style="font:600 11px ui-monospace,Menlo,monospace;letter-spacing:.12em;text-transform:uppercase;color:#CBD8A2;margin-bottom:10px">Locked</div>
<p style="margin:0">Open the link printed in the terminal where you started the Playbox UI. It carries the access token.</p></div>"""

ICON = (Path(__file__).resolve().parent / "static" / "brand" / "icon.svg").read_text()



class _QuickBind(http.server.ThreadingHTTPServer):
    """Skip `socket.getfqdn()` in `server_bind`: a reverse-DNS lookup that can stall startup for many seconds and is only used in logs."""

    def server_bind(self):
        import socketserver
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = str(self.server_address[0]), self.server_address[1]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=7717)
    ap.add_argument("--lan", action="store_true", help="listen on all interfaces so a phone on the same network can connect")
    ap.add_argument("--home", help="PLAYBOX_HOME for this sidecar (keys, history, room state)")
    ap.add_argument("--token", help="fixed access token (default: random each start)")
    ap.add_argument("--mcp", default=os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp") or shutil.which("playbox"),
                    help="path to playbox-mcp, or to playbox (then `playbox mcp` is run)")
    a = ap.parse_args()
    if not a.mcp:
        sys.exit("playbox not found. Install it: curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh")
    token = a.token or secrets.token_urlsafe(16)
    page_path = HERE / "static" / "index.html"
    page_path.read_text()  # fail early if missing
    sidecar = Sidecar(a.mcp, a.home)
    host = "0.0.0.0" if a.lan else "127.0.0.1"
    srv = _QuickBind((host, a.port), make_handler(sidecar, token, page_path, a.home or os.environ.get("PLAYBOX_HOME") or str(Path.home() / ".playbox")))
    print(f"Playbox UI  http://127.0.0.1:{srv.server_address[1]}/?t={token}", flush=True)
    if a.lan:
        print(f"On your phone  http://{lan_ip()}:{srv.server_address[1]}/?t={token}   (same Wi-Fi; the token is the key)", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        sidecar.close()


if __name__ == "__main__":
    main()
