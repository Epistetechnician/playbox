"""Tests for VerifiedKVGateway + aiproxy receipt binding."""
from __future__ import annotations

from dataclasses import dataclass

from playbox_router.aiproxy import compute_ledger_sha256, summarize_ledger_for_receipt
from playbox_router.kv_gateway import VerifiedKVGateway


@dataclass
class _DummyCluster:
    inter_key: str
    root_key: str
    text: str
    token_count: int
    slot_count: int


def test_kv_gateway_entity_routing_and_receipt_binding():
    clusters = [
        _DummyCluster("__preamble__", "__preamble__", "Preamble.", 10, 10),
        _DummyCluster("ACME-100", "crm", "ACME-100 SLA-7000 owner rep_A10", 80, 32),
        _DummyCluster("ACME-101", "crm", "ACME-101 SLA-7037 owner rep_B11", 85, 32),
    ]
    gw = VerifiedKVGateway(
        clusters,
        {
            "model_id": "Qwen/Qwen3-1.7B-Base",
            "tensors_sha256": "a" * 64,
            "pq_verified": True,
            "quarantined_dropped": 3,
        },
        entity_aliases={"ACME-100": ["account_0", "rep_A10"]},
    )

    sel, dec = gw.route("For account_0, what is the SLA code?")
    assert dec.entity == "ACME-100"
    assert dec.selected_clusters == ("__preamble__", "ACME-100")
    assert dec.injected_kv_slots == 42
    assert dec.pq_verified is True
    assert dec.quarantined_dropped == 3
    assert len(sel) == 2

    ledger = {
        "schema": "playbox.llm_ledger.v1",
        "room_id": "room-1",
        "run_id": "run-1",
        "cache_mode": "off",
        "failover_policy": "same_model_only",
        "totals": {"requests": 1, "input_tokens": 20, "output_tokens": 5, "cost_usd": 0.001},
    }
    ledger["ledger_sha256"] = compute_ledger_sha256(ledger)
    summary = summarize_ledger_for_receipt(ledger, kv_summary=gw.receipt_summary())
    assert summary["kv_gateway"]["pq_verified"] is True
    assert summary["kv_gateway"]["routes_served"] == 1
    assert summary["kv_gateway"]["total_kv_slots_injected"] == 42
    assert summary["kv_gateway"]["shard_tensors_sha256"] == "a" * 64
