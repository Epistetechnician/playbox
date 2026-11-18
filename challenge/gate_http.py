"""Minimal HTTP front for the Tier A merge gate, meant to sit behind attested-tls-proxy inside a CVM.

    GET  /status          frontier JSON
    GET  /verify          receipt-chain verification
    POST /submit/a        {"author": "...", "scheme": {...}}  -> gate verdict line

Shells out to gate.py so the gate's behaviour is identical to the CLI. One submission at a time (the gate mutates the
frontier). Stdlib only; binds to loopback by default because the attested proxy is the only intended client.
Tier B is not served: it needs torch and the model.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
GATE = HERE / "gate.py"
PROBLEMS = {
    "matmul2": HERE / "tier_a_matmul" / "problem.json",
    "matmul3": HERE / "tier_a_matmul" / "matmul3.json",
}
LOCK = threading.Lock()
MAX_BODY = 1 << 20
AUTHOR_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]{0,63}$")


def run_gate(*args: str, timeout: int = 120) -> tuple[int, str, str]:
    p = subprocess.run([sys.executable, str(GATE), *args], capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout.strip(), p.stderr.strip()


class Handler(BaseHTTPRequestHandler):
    timeout = 15

    def _send(self, code: int, obj) -> None:
        data = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path not in ("/status", "/verify"):
            return self._send(404, {"error": "not found"})
        if self.path == "/verify" and not (HERE / "state" / "receipts.jsonl").exists():
            return self._send(200, {"result": "no receipts yet"})
        with LOCK:
            rc, out, err = run_gate(self.path[1:])
        if rc != 0:
            return self._send(500, {"error": err or out})
        if self.path == "/status":
            try:
                return self._send(200, json.loads(out))
            except ValueError:
                return self._send(500, {"error": "invalid status JSON"})
        self._send(200, {"result": out})

    def do_POST(self):
        if self.path != "/submit/a":
            return self._send(404, {"error": "not found"})
        try:
            n = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            return self._send(400, {"error": "bad content-length"})
        if n <= 0 or n > MAX_BODY:
            return self._send(413 if n > MAX_BODY else 400, {"error": "bad body size"})
        try:
            req = json.loads(self.rfile.read(n))
            if not isinstance(req, dict):
                raise TypeError("body must be an object")
            author, scheme = req["author"], req["scheme"]
        except (ValueError, KeyError, TypeError):
            return self._send(400, {"error": "expected {author, scheme}"})
        if not isinstance(author, str) or not AUTHOR_RE.match(author):
            return self._send(400, {"error": "bad author"})
        if not isinstance(scheme, dict):
            return self._send(400, {"error": "scheme must be an object"})
        problem = req.get("problem", "matmul2")
        if not isinstance(problem, str) or problem not in PROBLEMS:
            return self._send(400, {"error": "bad problem"})
        with tempfile.TemporaryDirectory() as d, LOCK:
            path = Path(d) / "scheme.json"
            path.write_text(json.dumps(scheme))
            rc, out, err = run_gate("a", str(path), "--author", author, "--problem", str(PROBLEMS[problem]))
        self._send(200 if rc == 0 else 500, {"output": out if rc == 0 else (err or out)})

    def log_message(self, fmt, *a):
        sys.stderr.write("gate_http: " + fmt % a + "\n")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen", default="127.0.0.1:8081")
    host, port = ap.parse_args().listen.rsplit(":", 1)
    ThreadingHTTPServer((host, int(port)), Handler).serve_forever()


if __name__ == "__main__":
    main()
