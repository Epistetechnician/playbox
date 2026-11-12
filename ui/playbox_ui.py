#!/usr/bin/env python3
"""Playbox cockpit: a local web UI for a Playbox room, for desktop and phone.

Runs its own `playbox-mcp` sidecar (stdio) and serves a single-page app plus a small JSON API.
Stdlib only. Binds to 127.0.0.1 unless --lan is given; every request needs the access token.

    python3 ui/playbox_ui.py            # http://127.0.0.1:7717/?t=<token>
    python3 ui/playbox_ui.py --lan      # also reachable from a phone on the same network
"""
import argparse
import hmac
import http.server
import json
import os
import secrets
import shutil
import socket
import subprocess
import sys
import threading
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
        self.tools = []
        self._rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                 "clientInfo": {"name": "playbox-ui", "version": "0.1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.tools = [t["name"] for t in self._rpc("tools/list", {}).get("result", {}).get("tools", [])]

    def _send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def _rpc(self, method, params):
        self.next_id += 1
        want = self.next_id
        self._send({"jsonrpc": "2.0", "id": want, "method": method, "params": params})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("playbox-mcp exited")
            msg = json.loads(line)
            if msg.get("id") == want:
                return msg

    def call(self, name, args, wait=None):
        """Returns (is_error, parsed_json_or_text). `wait` seconds to get the lock, else (True, 'busy')."""
        if not self.lock.acquire(timeout=wait if wait is not None else -1):
            return True, "busy"
        try:
            if self.proc.poll() is not None:
                return True, "the playbox-mcp sidecar has exited; restart the UI"
            res = self._rpc("tools/call", {"name": name, "arguments": args}).get("result", {})
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
                    "background_color": "#19192f", "theme_color": "#19192f",
                    "icons": [{"src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any maskable"}]}),
                    "application/manifest+json")
            if url.path == "/icon.svg":
                return self._send(200, ICON, "image/svg+xml")
            if not self._authed():
                return self._send(401, LOCKED, "text/html; charset=utf-8")
            if url.path == "/":
                return self._send(200, page_path.read_text(), "text/html; charset=utf-8")
            if url.path == "/api/state":
                err, status = sidecar.call("playbox_status", {}, wait=2)
                return self._json(200, {"ok": not err, "status": status, "tools": sidecar.tools})
            if url.path == "/api/witness":
                return self._json(200, witness_state(home))
            self._send(404, "not found", "text/plain")

        def do_POST(self):
            if not self._authed():
                return self._json(401, {"error": "unauthorized"})
            if self.path == "/api/tamper":
                return self._tamper()
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
    """Holds and releases the Hermes witness plugin wrote, newest last. Read-only, never touches the sidecar."""
    path = Path(home) / "witness" / "holds.jsonl"
    events = []
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - 65536))
            for line in f.read().decode("utf-8", "replace").splitlines()[-200:]:
                try:
                    e = json.loads(line)
                    if isinstance(e, dict) and e.get("kind") in ("held", "released"):
                        events.append({k: e.get(k) for k in ("ts", "kind", "session", "tool", "origin")})
                except ValueError:
                    pass
    except OSError:
        pass
    last = {}
    for e in events:
        last[e["session"]] = e
    active = [e for e in last.values() if e["kind"] == "held"]
    return {"events": events[-50:], "active": active}


LOCKED = """<!doctype html><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1">
<body style="background:#19192f;color:#f2f2f2;font:16px system-ui;display:grid;place-items:center;height:100vh;margin:0">
<div style="max-width:28rem;padding:1.5rem"><h1 style="font-weight:600">Locked</h1>
<p>Open the link printed in the terminal where you started the Playbox UI. It carries the access token.</p></div>"""

ICON = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512"><rect width="512" height="512" fill="#19192f"/>
<text x="256" y="360" text-anchor="middle" font-size="300" fill="#e8c872" font-family="serif">&#9764;</text></svg>"""


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
    srv = http.server.ThreadingHTTPServer((host, a.port), make_handler(sidecar, token, page_path, a.home or os.environ.get("PLAYBOX_HOME") or str(Path.home() / ".playbox")))
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
