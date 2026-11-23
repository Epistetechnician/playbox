"""Drop-in OpenAI-compatible HTTP proxy for Entity-Routed KV + Adaptive RAG.

Sits transparently in front of any vLLM / OpenAI-compatible server:
- POST /v1/chat/completions : routes to pinned per-entity KV prefix (with fuzzy match +
  session pronoun carry-over) and appends retrieved snippets on cross-entity/policy drift.
  Supports both ``stream: true`` (SSE passthrough) and ``stream: false`` (JSON).
- POST /v1/entities         : register or update an entity dossier + aliases at runtime.
- GET  /v1/entities         : list registered entity IDs and names.
- GET  /metrics             : routing counters (entity hits, carry-overs, RAG fallbacks, latency).
- GET  /health              : liveness check.

CLI usage:
    PYTHONPATH=steer python3 -m playbox_steer.kv_routed_proxy \\
        --upstream http://127.0.0.1:8000 --port 8080 --entities-json entities.json
"""
from __future__ import annotations

import argparse
import json
import threading
import time
import urllib.error
import urllib.request
from collections import Counter
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Tuple

from playbox_steer.kv_sota_bench import BM25ChunkRetriever, SotaEntityRouter


@dataclass
class EntityRecord:
    entity_id: str
    name: str
    dossier: str
    aliases: List[str] = field(default_factory=list)


class RoutedKVProxyState:
    """Thread-safe entity store, session tracker, router, and fallback retriever."""

    def __init__(
        self,
        upstream_url: str,
        entities: Optional[List[EntityRecord]] = None,
        extra_chunks: Optional[List[Tuple[str, str]]] = None,
        retriever_factory: Optional[Callable[[List[Tuple[str, str]]], Callable[[str], Tuple[List[str], Dict[str, float]]]]] = None,
        system_preamble: str = "You are a precise assistant. Answer using the provided context.",
        top_k: int = 5,
    ) -> None:
        self.upstream_url = upstream_url.rstrip("/")
        self.system_preamble = system_preamble
        self.top_k = top_k
        self._retriever_factory = retriever_factory or (lambda ch: BM25ChunkRetriever(ch, top_k=self.top_k))
        self._lock = threading.RLock()
        self._entities: Dict[str, EntityRecord] = {}
        self._extra_chunks: List[Tuple[str, str]] = list(extra_chunks or [])
        self._session_entity: Dict[str, str] = {}
        self._stats: Counter[str] = Counter()
        self._total_route_ms: float = 0.0
        self._router: Optional[SotaEntityRouter] = None
        self._retriever: Optional[Callable[[str], Tuple[List[str], Dict[str, float]]]] = None

        for rec in entities or []:
            self._entities[rec.entity_id] = rec
        self._rebuild_index()

    def _rebuild_index(self) -> None:
        alias_map = {eid: list(rec.aliases) for eid, rec in self._entities.items()}
        name_map = {eid: rec.name for eid, rec in self._entities.items()}
        self._router = SotaEntityRouter(name_map, alias_map) if name_map else None

        chunks: List[Tuple[str, str]] = list(self._extra_chunks)
        for eid, rec in self._entities.items():
            chunks.append((f"entity:{eid}", f"[{rec.name}]\n{rec.dossier}"))
        self._retriever = self._retriever_factory(chunks) if chunks else None

    def upsert_entity(self, rec: EntityRecord) -> None:
        with self._lock:
            self._entities[rec.entity_id] = rec
            self._rebuild_index()

    def list_entities(self) -> List[Dict[str, Any]]:
        with self._lock:
            return [
                {"entity_id": r.entity_id, "name": r.name, "aliases": r.aliases, "dossier_chars": len(r.dossier)}
                for r in self._entities.values()
            ]

    def metrics(self) -> Dict[str, Any]:
        with self._lock:
            total = self._stats["requests"]
            return {
                "requests": total,
                "entity_hit": self._stats["entity_hit"],
                "session_carry": self._stats["session_carry"],
                "rag_fallback": self._stats["rag_fallback"],
                "passthrough": self._stats["passthrough"],
                "mean_route_ms": round(self._total_route_ms / total, 3) if total else 0.0,
                "active_sessions": len(self._session_entity),
                "registered_entities": len(self._entities),
            }

    def _resolve_entity(self, query: str, session_id: Optional[str]) -> Tuple[Optional[str], str]:
        if not self._entities or self._router is None:
            return None, "passthrough"
        active = self._session_entity.get(session_id) if session_id else None
        eid, mode = self._router.resolve(query, active_entity=active)
        if eid is not None:
            return eid, ("session_carry" if mode in ("session_carry", "pronoun_carry") else "entity_hit")
        return None, "rag_fallback"

    def rewrite_payload(
        self, payload: Dict[str, Any], session_id: Optional[str] = None
    ) -> Tuple[Dict[str, Any], Dict[str, str]]:
        """Inject pinned entity KV prefix or adaptive RAG fallback into chat messages."""
        t0 = time.perf_counter()
        messages = list(payload.get("messages") or [])
        last_user_idx = -1
        for i in range(len(messages) - 1, -1, -1):
            if messages[i].get("role") == "user":
                last_user_idx = i
                break

        if last_user_idx < 0:
            with self._lock:
                self._stats["requests"] += 1
                self._stats["passthrough"] += 1
            return payload, {"X-Routed-Mode": "passthrough", "X-Routed-Entity": ""}

        user_query = str(messages[last_user_idx].get("content") or "")
        with self._lock:
            eid, mode = self._resolve_entity(user_query, session_id)
            if eid is not None and session_id:
                self._session_entity[session_id] = eid
            rec = self._entities.get(eid) if eid is not None else None
            retriever = self._retriever

        out_messages = [dict(m) for m in messages]
        if rec is not None:
            prefix = f"{self.system_preamble}\n\n=== ENTITY DOSSIER: {rec.name} ===\n{rec.dossier}"
            if out_messages and out_messages[0].get("role") == "system":
                out_messages[0]["content"] = f"{prefix}\n\n{out_messages[0].get('content', '')}"
            else:
                out_messages.insert(0, {"role": "system", "content": prefix})
        elif mode == "rag_fallback" and retriever is not None:
            docs, _ = retriever(user_query)
            if docs:
                ctx = "\n\n---\n\n".join(docs)
                out_messages[last_user_idx]["content"] = (
                    f"Retrieved context:\n{ctx}\n\nQuestion:\n{user_query}"
                )

        elapsed_ms = (time.perf_counter() - t0) * 1000.0
        with self._lock:
            self._stats["requests"] += 1
            self._stats[mode] += 1
            self._total_route_ms += elapsed_ms

        rewritten = dict(payload)
        rewritten["messages"] = out_messages
        headers = {
            "X-Routed-Mode": mode,
            "X-Routed-Entity": eid or "",
            "X-Route-Ms": f"{elapsed_ms:.2f}",
        }
        return rewritten, headers


def create_proxy_server(
    host: str,
    port: int,
    state: RoutedKVProxyState,
) -> ThreadingHTTPServer:
    """Create a threaded HTTP server implementing the OpenAI-compatible routed KV proxy."""

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, format: str, *args: Any) -> None:
            return

        def _send_json(self, code: int, obj: Any, extra_headers: Optional[Dict[str, str]] = None) -> None:
            body = json.dumps(obj).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            for k, v in (extra_headers or {}).items():
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:
            if self.path == "/health":
                self._send_json(200, {"status": "ok", "upstream": state.upstream_url})
            elif self.path == "/metrics":
                self._send_json(200, state.metrics())
            elif self.path == "/v1/entities":
                self._send_json(200, {"entities": state.list_entities()})
            else:
                self._send_json(404, {"error": "not found"})

        def do_POST(self) -> None:
            length = int(self.headers.get("Content-Length", "0"))
            raw = self.rfile.read(length) if length > 0 else b"{}"
            try:
                payload = json.loads(raw.decode("utf-8"))
            except json.JSONDecodeError as exc:
                self._send_json(400, {"error": f"invalid json: {exc}"})
                return

            if self.path == "/v1/entities":
                eid = str(payload.get("entity_id") or "").strip()
                name = str(payload.get("name") or eid).strip()
                dossier = str(payload.get("dossier") or "")
                aliases = [str(a) for a in (payload.get("aliases") or [])]
                if not eid:
                    self._send_json(400, {"error": "entity_id is required"})
                    return
                state.upsert_entity(EntityRecord(entity_id=eid, name=name, dossier=dossier, aliases=aliases))
                self._send_json(200, {"status": "registered", "entity_id": eid})
                return

            if self.path != "/v1/chat/completions":
                self._send_json(404, {"error": "not found"})
                return

            session_id = self.headers.get("X-Session-Id") or payload.pop("session_id", None)
            rewritten, route_headers = state.rewrite_payload(payload, session_id=session_id)
            is_stream = bool(rewritten.get("stream", False))

            req = urllib.request.Request(
                f"{state.upstream_url}/v1/chat/completions",
                data=json.dumps(rewritten).encode("utf-8"),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            try:
                resp = urllib.request.urlopen(req, timeout=300.0)
            except urllib.error.HTTPError as exc:
                err_body = exc.read()
                self.send_response(exc.code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(err_body)))
                for k, v in route_headers.items():
                    self.send_header(k, v)
                self.end_headers()
                self.wfile.write(err_body)
                return
            except Exception as exc:
                self._send_json(502, {"error": f"upstream error: {exc}"}, route_headers)
                return

            with resp:
                if is_stream:
                    self.send_response(resp.status)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.send_header("Connection", "close")
                    for k, v in route_headers.items():
                        self.send_header(k, v)
                    self.end_headers()
                    while True:
                        chunk = resp.readline()
                        if not chunk:
                            break
                        self.wfile.write(chunk)
                        self.wfile.flush()
                    self.close_connection = True
                else:
                    body = resp.read()
                    self.send_response(resp.status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    for k, v in route_headers.items():
                        self.send_header(k, v)
                    self.end_headers()
                    self.wfile.write(body)

    return ThreadingHTTPServer((host, port), Handler)


def load_entities_file(path: Path) -> List[EntityRecord]:
    raw = json.loads(path.read_text(encoding="utf-8"))
    items = raw if isinstance(raw, list) else raw.get("entities", [])
    return [
        EntityRecord(
            entity_id=str(it["entity_id"]),
            name=str(it.get("name") or it["entity_id"]),
            dossier=str(it.get("dossier") or ""),
            aliases=[str(a) for a in (it.get("aliases") or [])],
        )
        for it in items
    ]


def main() -> None:
    p = argparse.ArgumentParser(description="Entity-Routed KV + Adaptive RAG OpenAI Proxy")
    p.add_argument("--upstream", default="http://127.0.0.1:8000")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=8080)
    p.add_argument("--entities-json", type=str, default=None)
    p.add_argument("--top-k", type=int, default=5)
    args = p.parse_args()

    entities = load_entities_file(Path(args.entities_json)) if args.entities_json else []
    state = RoutedKVProxyState(upstream_url=args.upstream, entities=entities, top_k=args.top_k)
    srv = create_proxy_server(args.host, args.port, state)
    print(f"kv_routed_proxy listening on http://{args.host}:{args.port} -> {args.upstream} ({len(entities)} entities)", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()


if __name__ == "__main__":
    main()
