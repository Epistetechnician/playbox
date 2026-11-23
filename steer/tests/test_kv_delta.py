"""Unit and HTTP integration tests for Delta-Framed Entity KV + Auto-Crossover Router."""
from __future__ import annotations

import json
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Dict, List

from playbox_steer.kv_delta import (
    AutoCrossoverPolicy,
    DeltaFramedEntity,
    prefix_preserved_ratio,
)
from playbox_steer.kv_routed_proxy import (
    EntityRecord,
    RoutedKVProxyState,
    create_proxy_server,
)


def _make_large_dossier(entity_id: str, name: str, approx_tokens: int) -> str:
    header = (
        f"=== ACCOUNT DOSSIER: {name} ({entity_id}) ===\n"
        f"Account Name: {name}\n"
        f"Account ID: {entity_id}\n"
        f"Annual Recurring Revenue: $1,250,000\n"
        f"Support Tier: Platinum\n"
        f"SLA Target: 99.90%\n"
        f"Primary Contact: Morgan Vance\n"
    )
    target_chars = approx_tokens * 4
    lines = [header]
    i = 0
    while sum(len(x) for x in lines) < target_chars:
        lines.append(
            f"[Audit #{i:04d}] Account {entity_id} routine quarterly compliance check "
            f"verified standard backup replication and billing ledger reconciliation.\n"
        )
        i += 1
    lines.append(f"Special Clause 99: Emergency escalation hotline is EXT-8842 for {name}.\n")
    return "".join(lines)


def test_delta_append_preserves_100pct_base_prefix_vs_naive_rewrite():
    base = _make_large_dossier("ACC-1", "Apex Corp", approx_tokens=4000)
    ent = DeltaFramedEntity("ACC-1", "Apex Corp", base, compact_delta_ratio=0.50, compact_max_stale=10)

    p0 = ent.render_full_prefix("SYS")
    # Naive in-place rewrite mutates SLA near the top -> destroys >98% of prefix bytes!
    naive_mutated = p0.replace("SLA Target: 99.90%", "SLA Target: 99.99%", 1)
    assert prefix_preserved_ratio(p0, naive_mutated) < 0.05

    # Delta-framed append preserves 100.0% of p0 bytes verbatim!
    res1 = ent.append_delta("SLA Target", "99.99%", note="Upgraded Q2")
    p1 = ent.render_full_prefix("SYS")
    assert res1["compacted"] is False
    assert res1["prefix_preserved_ratio"] == 1.0
    assert p1.startswith(p0)
    assert "[DELTA #001" in p1
    assert "SLA Target: 99.99%" in p1

    # Second mutation preserves 100.0% of p1 as well!
    res2 = ent.append_delta("SLA Target", "99.995%", note="Executive SLA")
    p2 = ent.render_full_prefix("SYS")
    assert res2["prefix_preserved_ratio"] == 1.0
    assert p2.startswith(p1)
    assert "supersedes: 99.99%" in p2


def test_cost_aware_compaction_triggers_on_stale_overwrites_or_ratio():
    base = _make_large_dossier("ACC-2", "Nimbus Labs", approx_tokens=1000)
    ent = DeltaFramedEntity("ACC-2", "Nimbus Labs", base, compact_delta_ratio=0.90, compact_max_stale=3)

    # 1st write sets active value (0 stale), next 3 overwrites hit stale_overwrites==3 -> auto-compact!
    assert ent.append_delta("SLA Target", "99.91%")["compacted"] is False
    assert ent.append_delta("SLA Target", "99.92%")["compacted"] is False
    assert ent.append_delta("SLA Target", "99.93%")["compacted"] is False
    r4 = ent.append_delta("SLA Target", "99.99%")
    assert r4["compacted"] is True
    assert "stale_overwrites" in r4["compact_reason"]
    assert ent.compaction_count == 1
    assert len(ent.delta_frames) == 0
    assert "SLA Target: 99.99%" in ent.base_snapshot
    assert "SLA Target: 99.90%" not in ent.base_snapshot


def test_auto_crossover_policy_classifies_all_three_tiers():
    policy = AutoCrossoverPolicy(l_full_max_tokens=13200, l_hybrid_switch_tokens=24000, l_wall_tokens=48000)

    small = DeltaFramedEntity("E-8K", "Small Co", _make_large_dossier("E-8K", "Small Co", 8000))
    medium = DeltaFramedEntity("E-32K", "Medium Co", _make_large_dossier("E-32K", "Medium Co", 32000))
    huge = DeltaFramedEntity("E-64K", "Huge Co", _make_large_dossier("E-64K", "Huge Co", 64000))

    d_small = policy.classify(small)
    d_med = policy.classify(medium)
    d_huge = policy.classify(huge)
    d_none = policy.classify(None)

    assert d_small.tier == "FULL-KV"
    assert d_med.tier == "HYBRID-HIER-KV"
    assert d_huge.tier == "HYBRID-HIER-KV"
    assert "above_kv_wall" in d_huge.reason
    assert d_none.tier == "CHUNKED-RAG"

    # Hierarchical skeleton stays compact (~2.5K tokens) even for a 64K dossier!
    hier_prefix = huge.render_hierarchical_prefix("SYS")
    assert len(hier_prefix) < 12000
    # Cold archive retriever finds deep-buried clause 99 inside the 64K dossier
    chunks = huge.retrieve_cold_chunks("What is the emergency escalation hotline?", top_k=3)
    assert any("EXT-8842" in c for c in chunks)


def test_proxy_http_delta_and_auto_crossover_end_to_end():
    captured: List[Dict[str, Any]] = []

    class MockUpstream(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: Any) -> None:
            return

        def do_POST(self) -> None:
            n = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(n).decode("utf-8"))
            captured.append(body)
            resp = json.dumps({"choices": [{"message": {"role": "assistant", "content": "ok"}}]}).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(resp)))
            self.end_headers()
            self.wfile.write(resp)

    up = ThreadingHTTPServer(("127.0.0.1", 0), MockUpstream)
    up_port = up.server_address[1]
    threading.Thread(target=up.serve_forever, daemon=True).start()

    entities = [
        EntityRecord("E-SMALL", "Aurora Health", _make_large_dossier("E-SMALL", "Aurora Health", 4000)),
        EntityRecord("E-HUGE", "Titan Global", _make_large_dossier("E-HUGE", "Titan Global", 56000)),
    ]
    state = RoutedKVProxyState(upstream_url=f"http://127.0.0.1:{up_port}", entities=entities)
    proxy = create_proxy_server("127.0.0.1", 0, state)
    p_port = proxy.server_address[1]
    threading.Thread(target=proxy.serve_forever, daemon=True).start()

    try:
        # 1. Small entity -> FULL-KV
        req1 = urllib.request.Request(
            f"http://127.0.0.1:{p_port}/v1/chat/completions",
            data=json.dumps({"model": "m", "messages": [{"role": "user", "content": "What is Aurora Health's SLA?"}]}).encode(),
            headers={"Content-Type": "application/json", "X-Session-Id": "s1"},
        )
        with urllib.request.urlopen(req1) as r1:
            assert r1.headers["X-Routed-Tier"] == "FULL-KV"
            assert r1.headers["X-Delta-Seq"] == "0"
        sys_before = captured[-1]["messages"][0]["content"]

        # 2. Live delta mutation via HTTP -> 100% prefix preservation!
        dreq = urllib.request.Request(
            f"http://127.0.0.1:{p_port}/v1/entities/delta",
            data=json.dumps({"entity_id": "E-SMALL", "field": "SLA Target", "value": "99.99%"}).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(dreq) as dr:
            dbody = json.loads(dr.read().decode())
            assert dbody["seq"] == 1
            assert dbody["prefix_preserved_ratio"] == 1.0

        req2 = urllib.request.Request(
            f"http://127.0.0.1:{p_port}/v1/chat/completions",
            data=json.dumps({"model": "m", "messages": [{"role": "user", "content": "And their updated SLA?"}]}).encode(),
            headers={"Content-Type": "application/json", "X-Session-Id": "s1"},
        )
        with urllib.request.urlopen(req2) as r2:
            assert r2.headers["X-Routed-Tier"] == "FULL-KV"
            assert r2.headers["X-Delta-Seq"] == "1"
        sys_after = captured[-1]["messages"][0]["content"]
        assert sys_after.startswith(sys_before)

        # 3. 56K huge entity -> automatically classified as HYBRID-HIER-KV (<12K prefix + cold archive chunk!)
        req3 = urllib.request.Request(
            f"http://127.0.0.1:{p_port}/v1/chat/completions",
            data=json.dumps({"model": "m", "messages": [{"role": "user", "content": "What is Titan Global's emergency escalation hotline?"}]}).encode(),
            headers={"Content-Type": "application/json", "X-Session-Id": "s2"},
        )
        with urllib.request.urlopen(req3) as r3:
            assert r3.headers["X-Routed-Tier"] == "HYBRID-HIER-KV"
        huge_req = captured[-1]["messages"]
        assert len(huge_req[0]["content"]) < 12000
        assert "EXT-8842" in huge_req[-1]["content"]
    finally:
        proxy.shutdown()
        proxy.server_close()
        up.shutdown()
        up.server_close()
