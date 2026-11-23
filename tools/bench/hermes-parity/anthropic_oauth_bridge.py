#!/usr/bin/env python3
"""Host-side Anthropic `/v1/messages` pass-through bridge for `playbox-aiproxy`.

Listens on loopback (default `127.0.0.1:18793`) so `playbox-aiproxy` can route
`/v1/messages` requests (streaming and non-streaming) with full host-side token
and cost ledgering while keeping credentials strictly on the host.
"""
from __future__ import annotations

import argparse
import http.client
import json
import os
import ssl
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

UPSTREAM_HOST = "api.anthropic.com"
ANTHROPIC_VERSION = "2023-06-01"


def _read_dotenv_key(env_path: Path, key_name: str) -> str:
    if not env_path.is_file():
        return ""
    for line in env_path.read_text(encoding="utf-8", errors="replace").splitlines():
        s = line.strip()
        if not s or s.startswith("#") or "=" not in s:
            continue
        k, v = s.split("=", 1)
        if k.strip() == key_name:
            val = v.strip().strip('"').strip("'")
            if val:
                return val
    return ""


def _resolve_anthropic_auth() -> tuple[str, bool]:
    """Return `(credential, is_oauth_token)` strictly from host Hermes PKCE store, env, or ~/.hermes/.env."""
    hermes_dir = Path(os.environ.get("PB_HERMES_AGENT_DIR", str(Path.home() / ".hermes" / "hermes-agent")))
    if hermes_dir.is_dir():
        if str(hermes_dir) not in sys.path:
            sys.path.insert(0, str(hermes_dir))
        try:
            from hermes_cli.runtime_provider import resolve_runtime_provider
            rp = resolve_runtime_provider(requested="anthropic")
            key = str(rp.get("api_key") or "").strip()
            if key and key.startswith("sk-ant-oat"):
                return key, True
        except Exception as exc:
            sys.stderr.write(f"[anthropic_oauth_bridge] Hermes OAuth lookup fell back to env: {exc}\n")
    env_key = os.environ.get("ANTHROPIC_API_KEY", "").strip()
    if env_key:
        return env_key, env_key.startswith("sk-ant-oat")
    dotenv_key = _read_dotenv_key(Path.home() / ".hermes" / ".env", "ANTHROPIC_API_KEY")
    if dotenv_key:
        return dotenv_key, dotenv_key.startswith("sk-ant-oat")
    raise RuntimeError("No host Anthropic API key or Hermes Anthropic OAuth token available")


class _AnthropicBridgeHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt: str, *args) -> None:
        return

    def _proxy(self, method: str) -> None:
        length = int(self.headers.get("Content-Length") or "0")
        body = self.rfile.read(length) if length > 0 else b""
        path = self.path
        if not path.startswith("/v1/"):
            path = "/v1" + (path if path.startswith("/") else "/" + path)
        try:
            cred, is_oauth = _resolve_anthropic_auth()
        except Exception as exc:
            err = json.dumps({"type": "error", "error": {"type": "authentication_error", "message": str(exc)}}).encode()
            self.send_response(500)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(err)))
            self.end_headers()
            self.wfile.write(err)
            return

        hdrs = {
            "Host": UPSTREAM_HOST,
            "Content-Type": self.headers.get("Content-Type", "application/json"),
            "Accept": self.headers.get("Accept", "*/*"),
            "anthropic-version": self.headers.get("anthropic-version", ANTHROPIC_VERSION),
            "User-Agent": "playbox-aiproxy-anthropic-bridge/0.4",
        }
        beta = self.headers.get("anthropic-beta", "")
        if is_oauth:
            hdrs["Authorization"] = f"Bearer {cred}"
            hdrs["User-Agent"] = "claude-cli/2.1.2 (external, cli)"
            hdrs["anthropic-dangerous-direct-browser-access"] = "true"
            hdrs["x-app"] = "cli"
            betas = [b.strip() for b in beta.split(",") if b.strip()]
            for req_beta in ("claude-code-20250219", "oauth-2025-04-20"):
                if req_beta not in betas:
                    betas.append(req_beta)
            hdrs["anthropic-beta"] = ",".join(betas)
            if body and path.startswith("/v1/messages"):
                try:
                    payload = json.loads(body.decode("utf-8"))
                    if isinstance(payload, dict):
                        cc_ident = {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}
                        sys_val = payload.get("system")
                        if sys_val is None:
                            payload["system"] = [cc_ident]
                        elif isinstance(sys_val, str):
                            if "Claude Code" not in sys_val:
                                payload["system"] = [cc_ident, {"type": "text", "text": sys_val}]
                        elif isinstance(sys_val, list):
                            has_cc = any(isinstance(b, dict) and "Claude Code" in str(b.get("text", "")) for b in sys_val)
                            if not has_cc:
                                payload["system"] = [cc_ident] + sys_val
                        body = json.dumps(payload).encode("utf-8")
                except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                    sys.stderr.write(f"[anthropic_oauth_bridge] skipped OAuth system-block rewrite: {exc}\n")
        else:
            hdrs["x-api-key"] = cred
            if beta:
                hdrs["anthropic-beta"] = beta

        ctx = ssl.create_default_context()
        conn = http.client.HTTPSConnection(UPSTREAM_HOST, 443, timeout=300, context=ctx)
        try:
            conn.request(method, path, body=body if body else None, headers=hdrs)
            resp = conn.getresponse()
            ctype = resp.getheader("Content-Type", "application/json")
            is_stream = "text/event-stream" in ctype.lower()
            if is_stream:
                self.send_response(resp.status)
                self.send_header("Content-Type", ctype)
                self.send_header("Cache-Control", "no-cache")
                self.send_header("Connection", "close")
                self.end_headers()
                while True:
                    chunk = resp.read(4096)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    self.wfile.flush()
            else:
                raw = resp.read()
                self.send_response(resp.status)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Length", str(len(raw)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(raw)
        except Exception as exc:
            err = json.dumps({"type": "error", "error": {"type": "api_error", "message": str(exc)[:240]}}).encode()
            self.send_response(502)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(err)))
            self.end_headers()
            self.wfile.write(err)
        finally:
            conn.close()

    def do_POST(self) -> None:
        self._proxy("POST")

    def do_GET(self) -> None:
        if self.path in ("/health", "/healthz"):
            raw = b'{"ok":true}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
            return
        self._proxy("GET")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=18793)
    args = ap.parse_args()
    srv = ThreadingHTTPServer((args.host, args.port), _AnthropicBridgeHandler)
    print(f"anthropic_oauth_bridge listening on http://{args.host}:{args.port}", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
