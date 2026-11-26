"""End-to-end tests for native Playbox `arc_maverick_proxy` (CPU-ARC Hybrid IS1 + Rust Maverick Sidecar + Ed25519 RoomLog)."""

from __future__ import annotations

import json
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

try:
    from playbox_steer.arc_maverick_proxy import ArcMaverickProxyState, create_arc_maverick_server
except ModuleNotFoundError as exc:
    if exc.name and exc.name.split(".")[0] == "arc_membrane":
        pytest.skip("requires optional sibling patelshaan/arc_membrane repo", allow_module_level=True)
    raise

from playbox_steer.kv_routed_proxy import EntityRecord

SIDECAR_BIN = Path(__file__).resolve().parents[3] / "patelshaan" / "arc_membrane" / "maverick_rs" / "target" / "release" / "sidecar"


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


@pytest.mark.skipif(not SIDECAR_BIN.exists(), reason=f"Missing optional Rust sidecar at {SIDECAR_BIN}")
def test_playbox_arc_maverick_proxy_e2e() -> None:
    assert SIDECAR_BIN.exists(), f"Missing Rust sidecar at {SIDECAR_BIN}"

    # 1. Start mock OpenAI upstream
    class MockUpstream(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: object) -> None:  # noqa: A002
            return

        def do_POST(self) -> None:  # noqa: N802
            length = int(self.headers.get("Content-Length", "0"))
            _ = self.rfile.read(length)
            body = json.dumps({
                "id": "up-1",
                "choices": [{"message": {"role": "assistant", "content": "upstream-ok"}}],
            }).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    up_srv = ThreadingHTTPServer(("127.0.0.1", 0), MockUpstream)
    up_port = up_srv.server_address[1]
    threading.Thread(target=up_srv.serve_forever, daemon=True).start()

    # 2. Start real Rust Maverick Verification-Only sidecar
    sidecar_port = _free_port()
    sidecar = subprocess.Popen(
        [str(SIDECAR_BIN), f"127.0.0.1:{sidecar_port}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(50):
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{sidecar_port}/health", timeout=0.5) as r:
                    if r.status == 200:
                        break
            except Exception:
                time.sleep(0.05)

        # 3. Start native Playbox ArcMaverickProxy
        state = ArcMaverickProxyState(
            upstream_url=f"http://127.0.0.1:{up_port}",
            maverick_sidecar_url=f"http://127.0.0.1:{sidecar_port}",
            entities=[EntityRecord(entity_id="acme", name="Acme Corp", dossier="Revenue is $42M.")],
        )
        srv = create_arc_maverick_server(state)
        port = srv.server_address[1]
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        base = f"http://127.0.0.1:{port}"

        # A. CPU-ARC local hit (0 upstream tokens)
        req1 = urllib.request.Request(
            f"{base}/v1/chat/completions",
            data=json.dumps({
                "messages": [{"role": "user", "content": "How far do I travel at 65 mph for 3 hours?"}],
                "arc_args": {"speed": 65, "hours": 3},
            }).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req1, timeout=10) as r1:
            assert r1.headers.get("X-Playbox-Route") == "cpu-arc-hit"
            d1 = json.loads(r1.read().decode("utf-8"))
            assert d1["choices"][0]["message"]["content"] == "195.0"
            assert d1["usage"]["total_tokens"] == 0

        # B. KV-Routed upstream fallback
        req2 = urllib.request.Request(
            f"{base}/v1/chat/completions",
            data=json.dumps({"messages": [{"role": "user", "content": "What is Acme Corp revenue?"}]}).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req2, timeout=10) as r2:
            assert r2.headers.get("X-Playbox-Route") == "kv-upstream"
            assert json.loads(r2.read().decode("utf-8"))["choices"][0]["message"]["content"] == "upstream-ok"

        # C. Live Rust Maverick honest (200) + cheat (400)
        req3 = urllib.request.Request(
            f"{base}/v1/maverick/verify",
            data=json.dumps({"m": 64, "n": 64, "seed": 7, "cheat": False}).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req3, timeout=10) as r3:
            assert r3.status == 200
            assert json.loads(r3.read().decode("utf-8"))["verified"] is True

        req4 = urllib.request.Request(
            f"{base}/v1/maverick/verify",
            data=json.dumps({"m": 64, "n": 64, "seed": 7, "cheat": True}).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            urllib.request.urlopen(req4, timeout=10)
            raise AssertionError("Expected 400 on Maverick cheat")
        except urllib.error.HTTPError as exc:
            assert exc.code == 400

        # D. Ed25519 RoomLog integrity
        with urllib.request.urlopen(f"{base}/v1/receipts", timeout=10) as r5:
            receipts = json.loads(r5.read().decode("utf-8"))
            assert receipts["valid"] is True
            assert len(receipts["entries"]) == 4

        srv.shutdown()
    finally:
        sidecar.terminate()
        sidecar.wait(timeout=5)
        up_srv.shutdown()
