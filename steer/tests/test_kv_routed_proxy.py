"""End-to-end tests for DenseRerankChunkRetriever and kv_routed_proxy."""
from __future__ import annotations

import json
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Dict, List

from playbox_steer.kv_routed_proxy import BoundedStickyRouter, EntityRecord, RoutedKVProxyState, create_proxy_server
from playbox_steer.kv_sota_bench import DenseRerankChunkRetriever


def test_dense_rerank_retriever_pipeline() -> None:
    chunks = [
        ("c1", "Apple FY2022 revenue was 394 billion dollars."),
        ("c2", "Microsoft Azure cloud growth reached 30 percent."),
        ("c3", "Boeing 737 MAX deliveries recovered in Q4."),
    ]
    vocab = ["apple", "revenue", "microsoft", "azure", "boeing", "deliveries"]

    def fake_embed(texts: List[str]) -> List[List[float]]:
        out = []
        for t in texts:
            tl = t.lower()
            out.append([1.0 if w in tl else 0.05 for w in vocab])
        return out

    def fake_rerank(pairs: List[tuple[str, str]]) -> List[float]:
        return [10.0 if "microsoft" in doc.lower() else 1.0 for _, doc in pairs]

    r = DenseRerankChunkRetriever(chunks, top_k=2, cand_k=3, embed_fn=fake_embed, rerank_fn=fake_rerank)
    docs, meta = r("What was Microsoft Azure growth?")
    assert len(docs) == 2
    assert "Microsoft Azure" in docs[0]
    assert meta["retrieve_ms"] >= 0.0 and meta["rerank_ms"] >= 0.0


def test_kv_routed_proxy_end_to_end_streaming_and_non_streaming() -> None:
    captured: List[Dict[str, Any]] = []

    class MockUpstream(BaseHTTPRequestHandler):
        def log_message(self, fmt: str, *args: Any) -> None:
            return

        def do_POST(self) -> None:
            n = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(n).decode("utf-8"))
            captured.append(body)
            sys_text = body["messages"][0]["content"] if body["messages"] else ""
            if body.get("stream"):
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                chunk = {"choices": [{"delta": {"content": f"SYS_LEN={len(sys_text)}"}}]}
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode("utf-8"))
                self.wfile.write(b"data: [DONE]\n\n")
            else:
                resp = json.dumps({"choices": [{"message": {"role": "assistant", "content": f"OK:{len(sys_text)}"}}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(resp)))
                self.end_headers()
                self.wfile.write(resp)

    upstream = ThreadingHTTPServer(("127.0.0.1", 0), MockUpstream)
    u_port = upstream.server_address[1]
    t_up = threading.Thread(target=upstream.serve_forever, daemon=True)
    t_up.start()

    state = RoutedKVProxyState(
        upstream_url=f"http://127.0.0.1:{u_port}",
        entities=[
            EntityRecord("acme", "Acme Corp", "Acme Corp renewal date is 2026-09-01. ARR is $420K.", aliases=["acme"]),
        ],
        extra_chunks=[("policy:sla", "Enterprise SLA policy guarantees 99.95% uptime.")],
    )
    proxy = create_proxy_server("127.0.0.1", 0, state)
    p_port = proxy.server_address[1]
    t_px = threading.Thread(target=proxy.serve_forever, daemon=True)
    t_px.start()

    try:
        # 1. Turn 1: explicit entity mention (non-streaming)
        req1 = urllib.request.Request(
            f"http://127.0.0.1:{p_port}/v1/chat/completions",
            data=json.dumps({"model": "test", "messages": [{"role": "user", "content": "What is Acme Corp renewal date?"}]}).encode(),
            headers={"Content-Type": "application/json", "X-Session-Id": "sess-1"},
        )
        with urllib.request.urlopen(req1) as r1:
            assert r1.headers["X-Routed-Mode"] == "entity_hit"
            assert r1.headers["X-Routed-Entity"] == "acme"
            data1 = json.loads(r1.read().decode())
            assert data1["choices"][0]["message"]["content"].startswith("OK:")
        assert "Acme Corp renewal date is 2026-09-01" in captured[-1]["messages"][0]["content"]

        # 2. Turn 2: implicit pronoun follow-up (streaming SSE)
        req2 = urllib.request.Request(
            f"http://127.0.0.1:{p_port}/v1/chat/completions",
            data=json.dumps({"model": "test", "stream": True, "messages": [{"role": "user", "content": "What is this account's ARR?"}]}).encode(),
            headers={"Content-Type": "application/json", "X-Session-Id": "sess-1"},
        )
        with urllib.request.urlopen(req2) as r2:
            assert r2.headers["X-Routed-Mode"] == "session_carry"
            assert r2.headers["X-Routed-Entity"] == "acme"
            sse = r2.read().decode()
            assert "SYS_LEN=" in sse and "[DONE]" in sse

        # 3. Fresh session with policy query -> adaptive RAG fallback
        req3 = urllib.request.Request(
            f"http://127.0.0.1:{p_port}/v1/chat/completions",
            data=json.dumps({"model": "test", "messages": [{"role": "user", "content": "What uptime does our SLA policy guarantee?"}]}).encode(),
            headers={"Content-Type": "application/json", "X-Session-Id": "sess-2"},
        )
        with urllib.request.urlopen(req3) as r3:
            assert r3.headers["X-Routed-Mode"] == "rag_fallback"
        assert "99.95% uptime" in captured[-1]["messages"][-1]["content"]

        # 4. Runtime entity registration + /metrics
        req_reg = urllib.request.Request(
            f"http://127.0.0.1:{p_port}/v1/entities",
            data=json.dumps({"entity_id": "globex", "name": "Globex Inc", "dossier": "Globex owner is Hank."}).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req_reg) as rr:
            assert json.loads(rr.read().decode())["entity_id"] == "globex"

        with urllib.request.urlopen(f"http://127.0.0.1:{p_port}/metrics") as rm:
            m = json.loads(rm.read().decode())
            assert m["requests"] == 3
            assert m["entity_hit"] == 1
            assert m["session_carry"] == 1
            assert m["rag_fallback"] == 1
            assert m["registered_entities"] == 2
    finally:
        proxy.shutdown()
        proxy.server_close()
        upstream.shutdown()
        upstream.server_close()


def test_bounded_sticky_router_spill_and_hot_replication() -> None:
    backends = [f"http://127.0.0.1:{8001 + i}" for i in range(4)]
    lb = BoundedStickyRouter(
        backends,
        policy="STICKY-BOUNDED",
        cap_factor=1.25,
        min_cap=2,
        hot_replicas=2,
        hot_share_threshold=0.35,
        hot_min_requests=6,
    )
    ring_cold = lb._pref_ring("cold_ent")
    # 1. Cold entity pins to primary up to min_cap=2, then spills to secondary
    i1, _, sp1, hot1 = lb.acquire("cold_ent")
    i2, _, sp2, hot2 = lb.acquire("cold_ent")
    i3, _, sp3, hot3 = lb.acquire("cold_ent")
    assert i1 == ring_cold[0] and i2 == ring_cold[0] and not sp1 and not sp2 and not hot1 and not hot2
    assert i3 == ring_cold[1] and sp3 and not hot3
    for idx in (i1, i2, i3):
        lb.release(idx)

    # 2. Hot entity crosses hot_share_threshold and stays within top-2 warm replicas without spilling
    ring_hot = lb._pref_ring("hot_ent")
    warm_set = set(ring_hot[:2])
    seen_hot_backends = set()
    hot_flag_seen = False
    for _ in range(10):
        idx, _, spilled, is_hot = lb.acquire("hot_ent")
        if is_hot:
            hot_flag_seen = True
            assert idx in warm_set
            assert not spilled
            seen_hot_backends.add(idx)
        lb.release(idx)
    assert hot_flag_seen
    assert seen_hot_backends == warm_set
    assert "hot_ent" in lb.stats()["hot_entities"]


def test_kv_routed_proxy_multi_replica_zipf_hot_replication() -> None:
    hits_by_port: Dict[int, int] = {}

    def make_handler(port_holder: List[int]):
        class MockReplica(BaseHTTPRequestHandler):
            def log_message(self, fmt: str, *args: Any) -> None:
                return

            def do_POST(self) -> None:
                n = int(self.headers.get("Content-Length", "0"))
                _ = self.rfile.read(n)
                hits_by_port[port_holder[0]] = hits_by_port.get(port_holder[0], 0) + 1
                resp = json.dumps({"choices": [{"message": {"role": "assistant", "content": f"PORT:{port_holder[0]}"}}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(resp)))
                self.end_headers()
                self.wfile.write(resp)

        return MockReplica

    servers = []
    urls = []
    for _ in range(4):
        holder = [0]
        srv = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(holder))
        holder[0] = srv.server_address[1]
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        servers.append(srv)
        urls.append(f"http://127.0.0.1:{holder[0]}")

    state = RoutedKVProxyState(
        upstreams=urls,
        entities=[
            EntityRecord("acme", "Acme Corp", "Acme dossier.", aliases=["acme"]),
            EntityRecord("globex", "Globex Inc", "Globex dossier.", aliases=["globex"]),
        ],
        routing_policy="STICKY-BOUNDED",
        hot_replicas=2,
        hot_share_threshold=0.40,
        hot_min_requests=4,
    )
    proxy = create_proxy_server("127.0.0.1", 0, state)
    p_port = proxy.server_address[1]
    threading.Thread(target=proxy.serve_forever, daemon=True).start()

    try:
        acme_backends = set()
        hot_seen = False
        for i in range(8):
            req = urllib.request.Request(
                f"http://127.0.0.1:{p_port}/v1/chat/completions",
                data=json.dumps({"model": "test", "messages": [{"role": "user", "content": "Tell me about Acme Corp"}]}).encode(),
                headers={"Content-Type": "application/json", "X-Session-Id": f"s-{i}"},
            )
            with urllib.request.urlopen(req) as r:
                acme_backends.add(r.headers["X-Routed-Backend"])
                if r.headers["X-Routed-Hot"] == "1":
                    hot_seen = True
        assert hot_seen
        assert len(acme_backends) == 2  # replicated across top-2 warm replicas, never scattered to all 4
        with urllib.request.urlopen(f"http://127.0.0.1:{p_port}/metrics") as rm:
            m = json.loads(rm.read().decode())
            assert m["routing_policy"] == "STICKY-BOUNDED"
            assert "acme" in m["hot_entities"]
            assert sum(m["replica_counts"]) == 8
    finally:
        proxy.shutdown()
        proxy.server_close()
        for srv in servers:
            srv.shutdown()
            srv.server_close()
