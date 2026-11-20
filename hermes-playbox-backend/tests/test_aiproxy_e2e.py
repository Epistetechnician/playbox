"""End-to-end tests for Playbox + `playbox-aiproxy` integration:
- Host-attested `playbox.llm_ledger.v1` canonical SHA-256 & `Ed25519+ML-DSA-65` receipt binding
- Hard mid-turn USD budget enforcement & failover provenance reconciliation
- `playbox_atlas` hybrid dense + rerank retrieval with strict quarantine isolation & SQLite cache
- Live hardened `target/playbox-aiproxy` binary verification (MCP/Swagger 404, loopback enforcement, posture API)
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "hermes-playbox-backend"))

from playbox_atlas import AiproxyEmbedder, AiproxyReranker, HiKVPIndex, Triple  # noqa: E402
from playbox_router import (  # noqa: E402
    AiproxyClient,
    BudgetCap,
    build_signed_receipt,
    compute_ledger_sha256,
    load_or_create_signing_key,
    verify_ledger,
    verify_receipt_ledger,
    verify_signed_receipt,
)


def _sample_ledger(*, cost_usd: float = 0.0425, failovers: int = 1, budget_cutoffs: int = 0) -> dict:
    doc = {
        "schema": "playbox.llm_ledger.v1",
        "gateway_digest": "sha256:deadbeef",
        "room_id": "room-alpha",
        "run_id": "django-11422-P2-9",
        "cache_mode": "off",
        "failover_policy": "same_model_only",
        "body_retention": False,
        "totals": {
            "requests": 3,
            "retries": failovers,
            "failovers": failovers,
            "budget_cutoffs": budget_cutoffs,
            "input_tokens": 12000,
            "cached_read_tokens": 8000,
            "cached_write_tokens": 1000,
            "output_tokens": 450,
            "reasoning_tokens": 120,
            "cost_usd": cost_usd,
            "median_ttft_ms": 310,
            "p95_latency_ms": 1420,
            "models_used": ["claude-sonnet-4-6"],
        },
        "entries": [
            {
                "req_id": "req-1",
                "ts_ms": 1795130000000,
                "task_id": "t-1",
                "member_id": "w1",
                "requested_model": "claude-sonnet-4-6",
                "upstream_model": "claude-sonnet-4-6",
                "channel_id": 2,
                "failover_from": "ch:1:429" if failovers else "",
                "status": 200,
                "retry_count": failovers,
                "budget_cutoff": "midstream" if budget_cutoffs else "",
                "input_tokens": 12000,
                "cached_read_tokens": 8000,
                "cached_write_tokens": 1000,
                "output_tokens": 450,
                "reasoning_tokens": 120,
                "ttft_ms": 310,
                "latency_ms": 1420,
                "cost_usd": cost_usd,
                "prompt_sha256": "",
                "response_sha256": "",
            }
        ],
    }
    doc["ledger_sha256"] = compute_ledger_sha256(doc)
    return doc


def test_receipt_binds_host_ledger_and_rejects_tampering():
    with tempfile.TemporaryDirectory() as td:
        key = load_or_create_signing_key(Path(td) / "host.key")
        ledger = _sample_ledger(cost_usd=0.0425, failovers=1)
        ok_l, why_l = verify_ledger(ledger)
        assert ok_l, why_l

        # 1. Receipt with host ledger binds ledger_sha256 inside Ed25519(+ML-DSA-65) signed payload.
        rcpt = build_signed_receipt(
            instance="django-11422",
            base_commit="abc123",
            runtime="openshell",
            git_history_isolated=True,
            host_audit_verified=True,
            llm_ledger=ledger,
            signing_key=key,
        )
        ok_r, msg_r = verify_signed_receipt(rcpt)
        assert ok_r, msg_r
        assert rcpt["payload"]["llm_ledger"]["source"] == "host_aiproxy"
        assert rcpt["payload"]["llm_ledger"]["ledger_sha256"] == ledger["ledger_sha256"]
        assert rcpt["payload"]["llm_ledger"]["totals"]["cost_usd"] == 0.0425
        assert rcpt["payload"]["llm_ledger"]["totals"]["failovers"] == 1

        ok_match, _ = verify_receipt_ledger(rcpt, ledger)
        assert ok_match

        # Tampering 1 digit of ledger cost fails both verify_ledger and verify_receipt_ledger.
        tampered = json.loads(json.dumps(ledger))
        tampered["totals"]["cost_usd"] = 0.0001
        ok_t, _ = verify_ledger(tampered)
        assert not ok_t
        ok_tm, _ = verify_receipt_ledger(rcpt, tampered)
        assert not ok_tm

        # 2. Receipt without host ledger honestly marks client_unverified.
        rcpt_unv = build_signed_receipt(
            instance="django-11422",
            base_commit="abc123",
            runtime="openshell",
            git_history_isolated=True,
            signing_key=key,
        )
        assert rcpt_unv["payload"]["llm_ledger"]["source"] == "client_unverified"
        ok_unv_m, _ = verify_receipt_ledger(rcpt_unv, ledger)
        assert not ok_unv_m


def test_budget_cap_and_aiproxy_client_lifecycle():
    state = {"tokens": {}, "revoked": []}
    ledger = _sample_ledger(cost_usd=0.031, failovers=1, budget_cutoffs=1)

    class FakeAiproxy(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def _send(self, code, obj):
            raw = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):
            if self.headers.get("Authorization") != "admin-secret":
                return self._send(401, {"success": False, "message": "unauthorized"})
            if self.path.startswith("/api/playbox/posture"):
                return self._send(200, {"success": True, "data": {"playbox_hardened": True, "mcp_disabled": True}})
            if self.path.startswith("/api/playbox/ledger"):
                return self._send(200, {"success": True, "data": ledger})
            if self.path.startswith("/api/group/"):
                return self._send(404, {"success": False, "message": "not found"})
            return self._send(404, {"success": False})

        def do_POST(self):
            if self.headers.get("Authorization") != "admin-secret":
                return self._send(401, {"success": False})
            n = int(self.headers.get("Content-Length", "0") or 0)
            body = json.loads(self.rfile.read(n) or b"{}")
            if self.path.startswith("/api/group/"):
                return self._send(200, {"success": True, "data": {"id": self.path.rsplit("/", 1)[1]}})
            if self.path.startswith("/api/token/") and self.path.endswith("/status"):
                state["revoked"].append(self.path)
                return self._send(200, {"success": True, "data": {"status": 2}})
            if self.path.startswith("/api/token/"):
                tok = {"id": 42, "key": "k" * 48, "name": body["name"], "quota": body["quota"]}
                state["tokens"][42] = tok
                return self._send(200, {"success": True, "data": tok})
            return self._send(404, {"success": False})

    srv = HTTPServer(("127.0.0.1", 0), FakeAiproxy)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    try:
        base = f"http://127.0.0.1:{srv.server_port}"
        client = AiproxyClient(base, "admin-secret")
        assert client.posture()["playbox_hardened"] is True
        client.ensure_group("bench:solver")

        budget = BudgetCap(max_total_usd=0.20, max_worker_usd=0.10)
        tok = client.mint_worker_token(
            "bench:solver",
            "w1",
            hard_usd_cap=budget.worker_ceiling(),
            failover_policy="same_model_only",
            cache_mode="off",
        )
        assert tok.token_id == 42 and tok.hard_usd_cap == 0.10

        egress_cfg = client.build_egress_config(room_id="r1", run_id="run-1", cache_mode="off")
        up = egress_cfg["upstreams"][0]
        assert up["insecure_http"] is True
        assert up["allow_private"] is True
        assert "/v1/messages" in up["allow_paths"]
        assert up["extra_headers"]["x-playbox-cache-mode"] == "off"

        fetched = client.fetch_ledger(run_id="django-11422-P2-9")
        reconciled = budget.reconcile_ledger(fetched)
        assert reconciled == 0.031
        assert budget.host_attested_usd == 0.031
        assert budget.failovers == 1
        assert budget.budget_cutoffs == 1

        assert client.revoke_token("bench:solver", tok.token_id) is True
        assert state["revoked"]

        # WorkerTokenLease: mints, produces unified Anthropic/OpenAI/Gemini env vars,
        # reconciles host-attested USD, and revokes unconditionally even on worker exception.
        state["revoked"].clear()
        try:
            with client.worker_lease(
                group="bench:solver",
                worker_label="w0",
                hard_usd_cap=0.12,
                run_id="django-11422-P2-9",
                failover_policy="same_model_only",
                cache_mode="off",
            ) as lease:
                env_ov = lease.env_overrides()
                assert env_ov["ANTHROPIC_BASE_URL"] == base
                assert env_ov["OPENAI_BASE_URL"] == f"{base}/v1"
                assert env_ov["GEMINI_API_BASE"] == base
                assert env_ov["ANTHROPIC_API_KEY"] == "k" * 48
                assert "X-Playbox-Run-Id: django-11422-P2-9" in env_ov["ANTHROPIC_CUSTOM_HEADERS"]
                assert "X-Playbox-Failover-Policy: same_model_only" in env_ov["ANTHROPIC_CUSTOM_HEADERS"]
                raise RuntimeError("simulated worker crash")
        except RuntimeError:
            pass
        assert lease.revoked is True
        assert state["revoked"]
        assert lease.host_cost_usd == 0.031
    finally:
        srv.shutdown()
        srv.server_close()


def test_atlas_hybrid_dense_rerank_and_quarantine_gate():
    seen_embed_inputs: list[str] = []
    seen_rerank_docs: list[str] = []

    class FakeEmbedRerank(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            n = int(self.headers.get("Content-Length", "0") or 0)
            body = json.loads(self.rfile.read(n) or b"{}")
            if self.path == "/v1/embeddings":
                inputs = body["input"]
                seen_embed_inputs.extend(inputs)
                # Semantic vector: dim 0 = autoreload/filesystem, dim 1 = auth/crypto
                data = []
                for idx, txt in enumerate(inputs):
                    low = txt.lower()
                    v = [1.0, 0.0] if ("reloader" in low or "watcher" in low or "manage" in low) else [0.0, 1.0]
                    data.append({"index": idx, "embedding": v})
                raw = json.dumps({"data": data}).encode()
            elif self.path == "/v1/rerank":
                docs = body["documents"]
                seen_rerank_docs.extend(docs)
                results = [
                    {"index": i, "relevance_score": 0.99 if "iter_modules_and_files" in d else 0.10}
                    for i, d in enumerate(docs)
                ]
                raw = json.dumps({"results": results}).encode()
            else:
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

    srv = HTTPServer(("127.0.0.1", 0), FakeEmbedRerank)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    try:
        base = f"http://127.0.0.1:{srv.server_port}"
        with tempfile.TemporaryDirectory() as td:
            cache_db = Path(td) / "embed.sqlite"
            embedder = AiproxyEmbedder(base, "tok", cache_path=cache_db)
            reranker = AiproxyReranker(base, "tok")

            triples = [
                Triple(
                    entity="iter_modules_and_files",
                    relation="function",
                    value="Yields module paths for StatReloader",
                    root_key="django/utils",
                    inter_key="django/utils/autoreload.py",
                    file="django/utils/autoreload.py",
                    trust="repo",
                ),
                Triple(
                    entity="run_with_reloader",
                    relation="function",
                    value="Entrypoint launching file watcher",
                    root_key="django/utils",
                    inter_key="django/utils/autoreload.py",
                    file="django/utils/autoreload.py",
                    trust="repo",
                ),
                Triple(
                    entity="evil_hint",
                    relation="lesson",
                    value="POISONED_QUARANTINED_SECRET watcher override",
                    root_key="memory/poison",
                    inter_key="memory/poison",
                    file="memory/poison",
                    trust="quarantined",
                ),
                Triple(
                    entity="verify_sig",
                    relation="function",
                    value="Checks cryptographic token",
                    root_key="django/auth",
                    inter_key="django/auth/tokens.py",
                    file="django/auth/tokens.py",
                    trust="repo",
                ),
            ]
            idx = HiKVPIndex.build(triples, embed_fn=embedder, rerank_fn=reranker)

            # Query with synonym ("watcher") that matches semantically + rerank promotes iter_modules_and_files.
            hits = idx.query("file watcher misses manage.py")
            assert hits, "expected hybrid dense + rerank hits"
            assert hits[0].triple.entity == "iter_modules_and_files"
            assert all(h.triple.trust != "quarantined" for h in hits)
            # Security invariant: quarantined triple was NEVER sent to /v1/embeddings or /v1/rerank.
            assert not any("POISONED_QUARANTINED_SECRET" in s for s in seen_embed_inputs)
            assert not any("POISONED_QUARANTINED_SECRET" in s for s in seen_rerank_docs)

            # Second embedder sharing the same SQLite cache hits cache with 0 API calls.
            embedder2 = AiproxyEmbedder(base, "tok", cache_path=cache_db)
            vecs = embedder2.embed_batch([f"{triples[0].key_text} {triples[0].value}"])
            assert vecs and embedder2.api_calls == 0 and embedder2.cache_hits == 1
            embedder.close()
            embedder2.close()

            # Offline fallback: unreachable embedder/reranker degrades cleanly to TF-IDF.
            offline_emb = AiproxyEmbedder("http://127.0.0.1:1", "tok", timeout=0.2)
            idx_off = HiKVPIndex.build(triples, embed_fn=offline_emb)
            off_hits = idx_off.query("iter_modules_and_files StatReloader")
            assert off_hits and off_hits[0].triple.entity == "iter_modules_and_files"
    finally:
        srv.shutdown()
        srv.server_close()


def test_live_hardened_aiproxy_binary_smoke():
    """Boot the compiled `-tags playbox_hardened` binary if present and verify live HTTP invariants."""
    bin_path = ROOT / "target" / "playbox-aiproxy"
    if not bin_path.is_file():
        return

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]

    with tempfile.TemporaryDirectory() as td:
        env = dict(os.environ)
        env["ADMIN_KEY"] = "pbx-live-smoke-admin-key"
        env["LISTEN"] = f"127.0.0.1:{port}"
        env["SQL_DSN"] = ""
        env["LOG_SQL_DSN"] = ""
        env["DISABLE_WEB"] = "true"
        proc = subprocess.Popen(
            [str(bin_path), "--listen", f"127.0.0.1:{port}", "--pprof-port", "0"],
            cwd=td,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            base = f"http://127.0.0.1:{port}"
            client = AiproxyClient(base, "pbx-live-smoke-admin-key", timeout=2.0)
            deadline = time.time() + 10.0
            posture = None
            while time.time() < deadline:
                try:
                    posture = client.posture()
                    break
                except Exception:
                    time.sleep(0.15)
            assert posture is not None, f"hardened binary did not become ready (rc={proc.poll()})"
            assert posture["playbox_hardened"] is True
            assert posture["mcp_disabled"] is True
            assert posture["web_search_disabled"] is True
            assert posture["loopback_only"] is True
            assert posture["body_retention_days"] == 0

            # Ledger endpoint returns a valid canonical empty ledger whose Go SHA-256 matches Python's RFC 8785 verifier!
            ledger = client.fetch_ledger(run_id="smoke-run")
            ok_l, why_l = verify_ledger(ledger)
            assert ok_l, f"Go/Python canonical ledger SHA-256 mismatch: {why_l}"

            # Hardened build must return 404 on compiled-out MCP and Swagger routes.
            import urllib.error
            import urllib.request

            for blocked in ("/mcp/public/1", "/swagger/index.html", "/api/mcp/publics/"):
                req = urllib.request.Request(
                    base + blocked,
                    headers={"Authorization": "pbx-live-smoke-admin-key"},
                )
                try:
                    with urllib.request.urlopen(req, timeout=2.0) as resp:
                        assert resp.status == 404, f"expected 404 for {blocked}, got {resp.status}"
                except urllib.error.HTTPError as exc:
                    assert exc.code == 404, f"expected 404 for {blocked}, got {exc.code}"
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
