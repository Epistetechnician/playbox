import io
import json
import sys
import threading
import urllib.error
import urllib.request
from email.message import Message
from http.server import ThreadingHTTPServer
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gate_http  # noqa: E402


def _serve():
    try:
        s = ThreadingHTTPServer(("127.0.0.1", 0), gate_http.Handler)
    except PermissionError:
        pytest.skip("loopback socket bind blocked in sandbox")
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s, f"http://127.0.0.1:{s.server_address[1]}"


def _post(url, body):
    r = urllib.request.Request(url, json.dumps(body).encode(), {"Content-Type": "application/json"})
    try:
        return urllib.request.urlopen(r).status
    except urllib.error.HTTPError as e:
        return e.code


def _invoke_handler(method: str, path: str, body: bytes = b"", headers: dict | None = None):
    h = gate_http.Handler.__new__(gate_http.Handler)
    h.path = path
    h.command = method
    h.request_version = "HTTP/1.1"
    h.rfile = io.BytesIO(body)
    h.wfile = io.BytesIO()
    hdrs = Message()
    for k, v in (headers or {}).items():
        hdrs[k] = v
    h.headers = hdrs
    captured = {}

    def _send(code, obj):
        captured["code"] = code
        captured["body"] = obj

    h._send = _send
    if method == "GET":
        h.do_GET()
    else:
        h.do_POST()
    return captured["code"], captured["body"]


def test_handler_unit_validation_and_stderr_isolation(monkeypatch):
    assert gate_http.Handler.timeout == 15
    # Malformed Content-Length returns 400 instead of 500 ValueError.
    code, body = _invoke_handler("POST", "/submit/a", b"{}", {"Content-Length": "not-an-int"})
    assert code == 400 and "content-length" in body["error"]
    # Oversized Content-Length returns 413.
    code, _ = _invoke_handler("POST", "/submit/a", b"{}", {"Content-Length": str(gate_http.MAX_BODY + 1)})
    assert code == 413
    # Leading '-' or '.' in author is rejected so CLI flags cannot be injected.
    for bad_author in ("-flag", "--author=bob", ".hidden", "../x", ""):
        raw = json.dumps({"author": bad_author, "scheme": {"products": []}}).encode()
        code, body = _invoke_handler("POST", "/submit/a", raw, {"Content-Length": str(len(raw))})
        assert code == 400 and body["error"] == "bad author"
    # Problem allowlist: matmul2 and matmul3 allowed, arbitrary paths rejected.
    raw = json.dumps({"author": "alice", "scheme": {"products": []}, "problem": "../etc/passwd"}).encode()
    code, body = _invoke_handler("POST", "/submit/a", raw, {"Content-Length": str(len(raw))})
    assert code == 400 and body["error"] == "bad problem"

    seen_args = []

    def fake_run_gate(*args, timeout=120):
        seen_args.append(args)
        if args == ("status",):
            return 0, '{"a": {"matmul2": []}}', "DeprecationWarning: noisy stderr"
        return 0, "REJECT receipt=1234", "some warning"

    monkeypatch.setattr(gate_http, "run_gate", fake_run_gate)
    code, body = _invoke_handler("GET", "/status")
    assert code == 200 and body == {"a": {"matmul2": []}}

    raw = json.dumps({"author": "alice_1", "scheme": {"products": []}, "problem": "matmul3"}).encode()
    code, body = _invoke_handler("POST", "/submit/a", raw, {"Content-Length": str(len(raw))})
    assert code == 200 and body == {"output": "REJECT receipt=1234"}
    assert seen_args[-1][-2:] == ("--problem", str(gate_http.PROBLEMS["matmul3"]))


def test_rejects_bad_requests():
    s, u = _serve()
    try:
        assert _post(u + "/submit/a", {"author": "x"}) == 400
        assert _post(u + "/submit/a", {"author": "../x", "scheme": {}}) == 400
        assert _post(u + "/submit/a", {"author": "-x", "scheme": {}}) == 400
        assert _post(u + "/nope", {}) == 404
    finally:
        s.shutdown()


def test_status():
    s, u = _serve()
    try:
        d = json.load(urllib.request.urlopen(u + "/status"))
        assert "a" in d
    finally:
        s.shutdown()
