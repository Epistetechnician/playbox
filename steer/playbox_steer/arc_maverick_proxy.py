"""Native Playbox OpenAI-Compatible Proxy combining:
  - Layer 1: Playbox RoutedKVProxy (`playbox_steer.kv_routed_proxy`) + Ed25519 Signed `RoomLog`
  - Layer 2: CPU-ARC `HybridSemanticRegistry` (BM25 + BGE-small + RRF) local bytecode short-circuit
  - Layer 3: Rust `maverick_rs` Goldilocks Verification-Only Sidecar (1 RTT matvec verification)

Endpoints:
  POST /v1/chat/completions -> CPU-ARC local hit (`X-Playbox-Route: cpu-arc-hit`) OR
                               RoutedKVProxy upstream passthrough (`X-Playbox-Route: kv-upstream`)
  POST /v1/maverick/verify  -> proxies to Rust `maverick_rs` Verification-Only sidecar & logs Ed25519 receipt
  GET  /v1/receipts         -> returns Ed25519 hash-chained `RoomLog` + verification flag
  GET  /metrics             -> routing & verification counters
  GET  /health              -> liveness
"""

from __future__ import annotations

import base64
import hashlib
import json
import sys
import threading
import time
import urllib.error
import urllib.request
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

from playbox_steer.kv_routed_proxy import EntityRecord, RoutedKVProxyState

# Allow importing `arc_membrane` cleanly from sibling repo when running inside `playbox`
_PATELSHAAN = Path(__file__).resolve().parents[3] / "patelshaan"
if _PATELSHAAN.exists() and str(_PATELSHAAN) not in sys.path:
    sys.path.insert(0, str(_PATELSHAAN))

from arc_membrane.is1_hybrid_retriever import HybridSemanticRegistry  # noqa: E402


class Ed25519RoomLog:
    """Thread-safe Ed25519-signed + SHA-256 hash-chained RoomLog for Playbox proxy receipts."""

    def __init__(self, room_id: str = "room:playbox-arc-maverick") -> None:
        self.room_id = room_id
        self._lock = threading.RLock()
        self._priv = Ed25519PrivateKey.generate()
        self.pub_bytes = self._priv.public_key().public_bytes_raw()
        self.entries: list[dict[str, Any]] = []
        self._prev_hash = "0" * 64

    def append(self, event_type: str, payload: dict[str, Any]) -> dict[str, Any]:
        with self._lock:
            body = {
                "room_id": self.room_id,
                "seq": len(self.entries),
                "event_type": event_type,
                "prev_hash": self._prev_hash,
                "payload": payload,
            }
            canonical = json.dumps(body, sort_keys=True, separators=(",", ":")).encode("utf-8")
            entry_hash = hashlib.sha256(canonical).hexdigest()
            sig = self._priv.sign(canonical)
            rec = {
                **body,
                "entry_hash": entry_hash,
                "ed25519_sig_b64": base64.b64encode(sig).decode("ascii"),
                "ed25519_pub_b64": base64.b64encode(self.pub_bytes).decode("ascii"),
            }
            self._prev_hash = entry_hash
            self.entries.append(rec)
            return rec

    def verify_all(self) -> bool:
        pub = Ed25519PublicKey.from_public_bytes(self.pub_bytes)
        with self._lock:
            prev = "0" * 64
            for i, rec in enumerate(self.entries):
                if rec["seq"] != i or rec["prev_hash"] != prev:
                    return False
                body = {
                    "room_id": rec["room_id"],
                    "seq": rec["seq"],
                    "event_type": rec["event_type"],
                    "prev_hash": rec["prev_hash"],
                    "payload": rec["payload"],
                }
                canonical = json.dumps(body, sort_keys=True, separators=(",", ":")).encode("utf-8")
                if hashlib.sha256(canonical).hexdigest() != rec["entry_hash"]:
                    return False
                try:
                    pub.verify(base64.b64decode(rec["ed25519_sig_b64"]), canonical)
                except Exception:
                    return False
                prev = rec["entry_hash"]
            return True


class ArcMaverickProxyState:
    """Combines Playbox RoutedKVProxyState + CPU-ARC Hybrid Registry + Maverick Sidecar + Ed25519 RoomLog."""

    def __init__(
        self,
        upstream_url: str,
        maverick_sidecar_url: str,
        entities: list[EntityRecord] | None = None,
        registry: HybridSemanticRegistry | None = None,
    ) -> None:
        self.kv_state = RoutedKVProxyState(upstream_url=upstream_url, entities=entities)
        self.maverick_sidecar_url = maverick_sidecar_url.rstrip("/")
        self.registry = registry or self._default_registry()
        self.room_log = Ed25519RoomLog()
        self._stats: Counter[str] = Counter()
        self._lock = threading.RLock()

    @staticmethod
    def _default_registry() -> HybridSemanticRegistry:
        reg = HybridSemanticRegistry(match_threshold=0.35, hybrid_bm25=True)
        reg.register(
            description="Calculate total distance traveled given speed in mph and time in hours",
            fn=lambda args: {"answer": float(args["speed"]) * float(args["hours"])},
            code_bytes=b"distance_rate_time",
            lcb_score=0.92,
        )
        reg.register(
            description="Calculate final price after percentage discount on an item",
            fn=lambda args: {"answer": round(float(args["price"]) * (1.0 - float(args["pct"]) / 100.0), 4)},
            code_bytes=b"percent_discount",
            lcb_score=0.91,
        )
        return reg

    def handle_chat(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any], dict[str, str]]:
        messages = payload.get("messages") or []
        user_text = ""
        for m in reversed(messages):
            if m.get("role") == "user":
                user_text = str(m.get("content", ""))
                break

        arc_args = payload.get("arc_args")
        if user_text and isinstance(arc_args, dict):
            sf = self.registry.resolve(user_text)
            if sf is not None:
                t0 = time.perf_counter()
                out = sf.fn(arc_args)
                result = out.get("answer", out) if isinstance(out, dict) else out
                exec_ms = (time.perf_counter() - t0) * 1000.0
                with self._lock:
                    self._stats["cpu_arc_hits"] += 1
                code_sha = hashlib.sha256(sf.code_bytes).hexdigest()
                self.room_log.append(
                    "cpu_arc_hit",
                    {"skill": sf.description[:48], "code_sha256": code_sha, "exec_ms": round(exec_ms, 4)},
                )
                return (
                    200,
                    {
                        "id": f"arc-{code_sha[:12]}",
                        "object": "chat.completion",
                        "model": "cpu-arc-local",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": str(result)}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
                    },
                    {"X-Playbox-Route": "cpu-arc-hit", "X-Playbox-Skill": code_sha[:12]},
                )

        # Fallback: Playbox RoutedKVProxy rewrite + upstream forward
        rewritten, headers = self.kv_state.rewrite_payload(payload)
        req = urllib.request.Request(
            f"{self.kv_state.upstream_url}/v1/chat/completions",
            data=json.dumps(rewritten).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=15) as resp:
            body = json.loads(resp.read().decode("utf-8"))
        with self._lock:
            self._stats["kv_upstream_calls"] += 1
        self.room_log.append("kv_upstream", {"route_headers": headers})
        return 200, body, {**headers, "X-Playbox-Route": "kv-upstream"}

    def handle_maverick_verify(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        req = urllib.request.Request(
            f"{self.maverick_sidecar_url}/verify_matvec",
            data=json.dumps(payload).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                status = resp.status
                body = json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as exc:
            status = exc.code
            body = json.loads(exc.read().decode("utf-8"))
        with self._lock:
            self._stats["maverick_verified" if status == 200 else "maverick_rejected"] += 1
        self.room_log.append("maverick_verify", {"status": status, "verified": body.get("verified", False)})
        return status, body

    def metrics(self) -> dict[str, Any]:
        with self._lock:
            return {
                **dict(self._stats),
                "kv_metrics": self.kv_state.metrics(),
                "room_log_entries": len(self.room_log.entries),
            }


def create_arc_maverick_server(state: ArcMaverickProxyState, host: str = "127.0.0.1", port: int = 0) -> ThreadingHTTPServer:
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
            return

        def _send(self, code: int, obj: dict[str, Any], extra_headers: dict[str, str] | None = None) -> None:
            raw = json.dumps(obj).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            for k, v in (extra_headers or {}).items():
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self) -> None:  # noqa: N802
            if self.path == "/health":
                self._send(200, {"status": "ok", "proxy": "playbox-arc-maverick"})
            elif self.path == "/metrics":
                self._send(200, state.metrics())
            elif self.path == "/v1/receipts":
                self._send(200, {"valid": state.room_log.verify_all(), "entries": state.room_log.entries})
            else:
                self._send(404, {"error": "not_found"})

        def do_POST(self) -> None:  # noqa: N802
            length = int(self.headers.get("Content-Length", "0"))
            payload = json.loads(self.rfile.read(length).decode("utf-8")) if length else {}
            if self.path == "/v1/chat/completions":
                code, body, hdrs = state.handle_chat(payload)
                self._send(code, body, hdrs)
            elif self.path == "/v1/maverick/verify":
                code, body = state.handle_maverick_verify(payload)
                self._send(code, body)
            else:
                self._send(404, {"error": "not_found"})

    return ThreadingHTTPServer((host, port), Handler)
