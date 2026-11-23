"""Unit and end-to-end mock-server tests for playbox_steer.kv_routed."""
from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Dict, List

import pytest

from playbox_steer import kv_routed as KR


def _mini_bundle() -> KR.CorpusBundle:
    docs = [
        {"doc_id": "cust-C01", "title": "Northwind Logistics", "kind": "customer", "customer_id": "C01",
         "text": "Northwind Logistics is on the Growth plan with 504 seats. Renewal quarter: Q1. ARR: $120,000.", "tokens": 30},
        {"doc_id": "alias-C01", "title": "Northwind Logistics Group", "kind": "alias", "customer_id": "C01-A",
         "text": "Northwind Logistics Group is on the Starter plan with 12 seats. Renewal quarter: Q4. ARR: $9,000.", "tokens": 30},
        {"doc_id": "cust-C02", "title": "Acme Robotics", "kind": "customer", "customer_id": "C02",
         "text": "Acme Robotics is on the Enterprise plan with 40 seats. Renewal quarter: Q2. ARR: $300,000.", "tokens": 30},
        {"doc_id": "tickets-C01-C02", "title": "Tickets C01 C02", "kind": "tickets", "customer_id": None,
         "text": "Northwind Logistics has 1 open P1 ticket. Acme Robotics has 0 open P1 tickets.", "tokens": 25},
        {"doc_id": "POL-RenewalRisk", "title": "Renewal Policy", "kind": "policy", "customer_id": None,
         "text": "Growth plan includes Standard support SLA.", "tokens": 15},
        {"doc_id": "fill-0001", "title": "Filler", "kind": "filler", "customer_id": None,
         "text": "General office filler " * 40, "tokens": 120},
    ]
    chunks = {
        f"ch-{i}": {"chunk_id": f"ch-{i}", "doc_id": d["doc_id"], "text": d["text"]}
        for i, d in enumerate(docs)
    }
    entities = [
        {"name": "Northwind Logistics", "doc_id": "cust-C01"},
        {"name": "Northwind Logistics Group", "doc_id": "alias-C01"},
        {"name": "Acme Robotics", "doc_id": "cust-C02"},
    ]
    queries = {
        "A-1": {"query_id": "A-1", "category": "A", "answer_type": "exact",
                "text": "What plan tier is Northwind Logistics on?", "answer": "Growth"},
        "A-2": {"query_id": "A-2", "category": "A", "answer_type": "exact",
                "text": "What plan tier is Northwind Logistics Group on?", "answer": "Starter"},
        "B-1": {"query_id": "B-1", "category": "B", "answer_type": "exact",
                "text": "Between Northwind Logistics and Acme Robotics, which renews in Q1?", "answer": "Northwind Logistics"},
        "D-1": {"query_id": "D-1", "category": "D", "answer_type": "rubric",
                "text": "Give renewal risk brief for Northwind Logistics.", "answer": "Standard"},
    }
    sessions = [
        {"session_id": "S-1", "customer_id": "C01", "turns": ["A-1", "B-1", "D-1"]},
        {"session_id": "S-2", "customer_id": "C01-A", "turns": ["A-2", "B-1"]},
    ]
    return KR.CorpusBundle(docs=docs, chunks=chunks, entities=entities, queries=queries, sessions=sessions)


def test_entity_router_longest_match_lookalikes():
    b = _mini_bundle()
    r = KR.EntityRouter(b.docs, b.entities)
    assert r.detect_entities("What plan is Northwind Logistics Group on?") == ["alias-C01"]
    assert r.detect_entities("What plan is Northwind Logistics on?") == ["cust-C01"]
    assert r.detect_entities("Compare Northwind Logistics and Acme Robotics.") == ["cust-C01", "cust-C02"]
    assert r.detect_entities("No known account mentioned here.") == []


def test_working_set_excludes_filler_and_lookalike():
    b = _mini_bundle()
    r = KR.EntityRouter(b.docs, b.entities)
    ws = r.working_set_text("cust-C01")
    assert "Northwind Logistics is on the Growth plan" in ws
    assert "1 open P1 ticket" in ws
    assert "Standard support SLA" in ws
    assert "Northwind Logistics Group" not in ws
    assert "General office filler" not in ws


def test_stats_helpers():
    assert KR.percentile_nearest([1.0, 2.0, 3.0, 4.0, 5.0], 95) == 5.0
    lo, hi = KR.bootstrap_ci([1.0, 2.0, 3.0, 4.0, 5.0], lambda xs: sum(xs) / len(xs), n_boot=200, seed=1)
    assert 1.0 <= lo <= hi <= 5.0
    mc = KR.mcnemar_exact({1: 1.0, 2: 1.0, 3: 1.0, 4: 0.0}, {1: 1.0, 2: 0.0, 3: 0.0, 4: 0.0})
    assert mc["n_paired"] == 4 and mc["a_only"] == 2 and mc["b_only"] == 0
    assert 0.0 < mc["p_value"] <= 1.0


def test_end_to_end_three_arms_mock_server():
    bundle = _mini_bundle()
    bm25 = KR.SimpleBM25(list(bundle.chunks.values()))
    seen_system_prefixes: Dict[str, List[str]] = {}
    cached_prefixes = set()
    lock = threading.Lock()

    class MockVLLM(BaseHTTPRequestHandler):
        def log_message(self, fmt: str, *args: Any) -> None:
            return

        def do_GET(self) -> None:
            if self.path == "/metrics":
                body = (
                    "vllm:prefix_cache_hits_total 1000\n"
                    "vllm:prefix_cache_queries_total 1000\n"
                    "vllm:external_prefix_cache_hits_total 0\n"
                    "vllm:external_prefix_cache_queries_total 10\n"
                ).encode()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(body)
                return
            self.send_response(404)
            self.end_headers()

        def do_POST(self) -> None:
            length = int(self.headers.get("Content-Length", "0"))
            req = json.loads(self.rfile.read(length).decode())
            msgs = req["messages"]
            sys_msg = msgs[0]["content"]
            prompt_toks = sum(len(m["content"].split()) for m in msgs)
            sys_toks = len(sys_msg.split())
            with lock:
                hit = sys_msg in cached_prefixes
                cached_prefixes.add(sys_msg)
                seen_system_prefixes.setdefault(sys_msg.splitlines()[0], []).append(sys_msg)
            cached = sys_toks if hit else 0
            # Echo ground-truth hint from prompt so scorer passes cleanly
            answer = "Answer: Growth" if "Growth" in json.dumps(msgs) else "Answer: Starter"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            ev1 = {"choices": [{"delta": {"content": answer}, "finish_reason": None}]}
            ev2 = {
                "choices": [{"delta": {}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": prompt_toks,
                    "completion_tokens": 3,
                    "prompt_tokens_details": {"cached_tokens": cached},
                },
            }
            self.wfile.write(f"data: {json.dumps(ev1)}\n\n".encode())
            self.wfile.write(f"data: {json.dumps(ev2)}\n\n".encode())
            self.wfile.write(b"data: [DONE]\n\n")

    server = ThreadingHTTPServer(("127.0.0.1", 0), MockVLLM)
    port = server.server_address[1]
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    try:
        base_url = f"http://127.0.0.1:{port}"
        scorer = lambda q, out: {"score": 1.0 if str(q["answer"]) in out else 0.0, "why": "mock"}
        cells = []
        for strat in ("S-RAG", "S-CTX", "S-ROUTED"):
            cell = KR.run_cell(
                bundle=bundle,
                strategy=strat,
                concurrency=2,
                block=1,
                base_url=base_url,
                model="mock-model",
                retriever=lambda q: bm25.search(q, k=2),
                scorer=scorer,
                max_tokens=32,
            )
            cells.append(cell)

        # Each arm got a unique UUID salt
        salts = {c["probe"]["salt"] for c in cells}
        assert len(salts) == 3

        summary = KR.summarize_cells(cells)
        assert set(summary.keys()) == {"S-RAG@n2", "S-CTX@n2", "S-ROUTED@n2"}
        # Routed working set must be much smaller than full corpus
        assert summary["S-ROUTED@n2"]["mean_prompt_tokens"] < summary["S-CTX@n2"]["mean_prompt_tokens"]
        # Routed arm must preserve its system prefix across turns (cache hit > 0 on turns 2+)
        assert summary["S-ROUTED@n2"]["cache_hit_pct"] > 0.0
        # Cross-account B-1 turn must have triggered adaptive fallback
        routed_turns = [t for c in cells if c["strategy"] == "S-ROUTED" for t in c["turns"]]
        b1_turns = [t for t in routed_turns if t["query"] == "B-1"]
        assert all(t["used_fallback"] for t in b1_turns)
        # Single-account A-1 turn should NOT trigger fallback
        a1_turns = [t for t in routed_turns if t["query"] == "A-1"]
        assert all(not t["used_fallback"] for t in a1_turns)
    finally:
        server.shutdown()


def test_real_crm_bundle_routing_coverage():
    """If the verified 128K CRM archive is present locally, verify zero-oracle routing on all 32 sessions."""
    bundle_dir = Path("/tmp/thread_check/logs")
    if not (bundle_dir / "corpus/v2/corpus_128k.jsonl").exists():
        pytest.skip("Local CRM bundle not present")
    bundle = KR.load_crm_bundle(bundle_dir, max_sessions=32)
    router = KR.EntityRouter(bundle.docs, bundle.entities)
    ws_tokens = []
    routed_ok = 0
    total_turns = 0
    for s in bundle.sessions:
        primary = None
        for idx, qid in enumerate(s["turns"], start=1):
            q = bundle.queries[qid]
            matched = router.detect_entities(q["text"])
            if idx == 1:
                primary = matched[0] if matched else None
                ws = router.working_set_text(primary)
                ws_tokens.append(len(ws) // 4)
            # Check that every required doc_id is covered by working set + extra_dossier
            ws_text = router.working_set_text(primary)
            extra = router.extra_dossier_text(matched, exclude_primary=primary)
            combined = ws_text + "\n" + extra
            req_ok = all(
                router.docs_by_id[did]["text"][:80] in combined
                for did in q.get("required_doc_ids", [])
                if did in router.docs_by_id
            )
            if req_ok:
                routed_ok += 1
            total_turns += 1
    avg_ws = sum(ws_tokens) / len(ws_tokens)
    assert avg_ws < 10_000, f"Working set too large: {avg_ws:.0f} est tokens"
    assert routed_ok / total_turns >= 0.95, f"Required doc coverage too low: {routed_ok}/{total_turns}"


def test_fuzzy_typo_and_noisy_bundle():
    bundle = _mini_bundle()
    router = KR.EntityRouter(bundle.docs, bundle.entities)
    # Single-char typo disambiguates cleanly; ambiguous double-typo across look-alikes safely falls back to RAG ([])
    assert router.detect_entities("What is the billing tier for Acme Robtics?") == ["cust-C02"]
    assert router.detect_entities("What is the SLA for Northwnd Logistics?") == ["cust-C01"]
    assert router.detect_entities("What is the SLA for Northwnd Logistics Grup?") == []

    noisy, stats = KR.corrupt_bundle_noisy(bundle, router, typo_turn1_frac=1.0, pronoun_followup_frac=1.0, seed=7)
    assert stats["typo_turn1"] >= 1
    assert noisy.queries["A-1"]["text"] != bundle.queries["A-1"]["text"]


def test_pronoun_carry_and_wave_eviction():
    bundle = _mini_bundle()
    # Add a pronoun-only follow-up on turn 2 and expand to 2 tenants
    bundle.queries["A-PRON"] = {
        "query_id": "A-PRON",
        "category": "A",
        "answer_type": "exact",
        "text": "What is this account's billing tier?",
        "answer": "Enterprise",
    }
    bundle.sessions[0]["turns"] = ["A-1", "A-PRON"]
    expanded = KR.expand_bundle_tenants(bundle, scale=2)
    assert len(expanded.sessions) == 4
    assert len({s["tenant_tag"] for s in expanded.sessions}) == 2

    bm25 = KR.SimpleBM25(list(expanded.chunks.values()))

    class MockVLLM(BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):
            pass

        def do_GET(self):
            if self.path == "/metrics":
                body = (
                    b"vllm:prefix_cache_hits_total 400\n"
                    b"vllm:prefix_cache_queries_total 500\n"
                    b"vllm:external_prefix_cache_hits_total 120\n"
                    b"vllm:external_prefix_cache_queries_total 200\n"
                )
                self.send_response(200)
                self.end_headers()
                self.wfile.write(body)

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            req = json.loads(self.rfile.read(length).decode("utf-8"))
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            ev1 = {"choices": [{"delta": {"content": "Answer: Enterprise"}, "finish_reason": None}]}
            ev2 = {
                "choices": [{"delta": {}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 50, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 25}},
            }
            self.wfile.write(f"data: {json.dumps(ev1)}\n\n".encode())
            self.wfile.write(f"data: {json.dumps(ev2)}\n\n".encode())
            self.wfile.write(b"data: [DONE]\n\n")

    server = ThreadingHTTPServer(("127.0.0.1", 0), MockVLLM)
    port = server.server_address[1]
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    try:
        cell = KR.run_cell(
            bundle=expanded,
            strategy="S-ROUTED",
            concurrency=2,
            block=1,
            base_url=f"http://127.0.0.1:{port}",
            model="mock-model",
            retriever=lambda q: bm25.search(q, k=2),
            max_tokens=16,
            wave_eviction=True,
        )
        # Wave eviction orders all turn-1 rows before turn-2 rows
        turn_nums = [r["turn"] for r in cell["turns"]]
        assert turn_nums[:4] == [1, 1, 1, 1]
        assert turn_nums[4:] == [2, 2, 2, 2]
        # Pronoun follow-up carried pinned primary_doc_id without triggering RAG fallback
        pron_rows = [r for r in cell["turns"] if r["query"] == "A-PRON"]
        assert len(pron_rows) == 2
        assert all(r["route_mode"] == "pronoun_carry" and not r["used_fallback"] for r in pron_rows)
    finally:
        server.shutdown()
