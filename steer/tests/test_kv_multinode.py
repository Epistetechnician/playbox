"""Unit tests for multi-replica sticky routing load balancer (RR vs STICKY-HASH vs STICKY-BOUNDED)."""
from __future__ import annotations

import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

from kv_multinode_run import ReplicaLoadBalancer, sample_session_entities


def test_rr_balances_perfectly_and_scatters_entity() -> None:
    lb = ReplicaLoadBalancer([f"http://127.0.0.1:800{i}" for i in range(1, 5)], policy="RR")
    picked = []
    for _ in range(8):
        idx, _, spilled = lb.acquire("ACME-100")
        assert not spilled
        picked.append(idx)
        lb.release(idx)
    assert picked == [0, 1, 2, 3, 0, 1, 2, 3]
    assert lb.imbalance_ratio() == 1.0


def test_sticky_hash_pins_same_entity_and_hotspots_on_zipf() -> None:
    backends = [f"http://127.0.0.1:800{i}" for i in range(1, 5)]
    lb = ReplicaLoadBalancer(backends, policy="STICKY-HASH")
    first_idx, _, _ = lb.acquire("ACME-100")
    for _ in range(15):
        idx, _, spilled = lb.acquire("ACME-100")
        assert idx == first_idx
        assert not spilled
    assert lb.imbalance_ratio() == 4.0


def test_sticky_bounded_stays_sticky_under_low_load_and_spills_on_hotspot() -> None:
    backends = [f"http://127.0.0.1:800{i}" for i in range(1, 5)]
    lb = ReplicaLoadBalancer(backends, policy="STICKY-BOUNDED", cap_factor=1.25, min_cap=2)
    # Under low load (<= min_cap), stays 100% sticky on primary replica
    idx0, _, s0 = lb.acquire("ACME-100")
    idx1, _, s1 = lb.acquire("ACME-100")
    assert idx0 == idx1
    assert not s0 and not s1
    # 3rd concurrent request for the same hot entity spills to secondary replica
    idx2, _, s2 = lb.acquire("ACME-100")
    assert idx2 != idx0
    assert s2 is True
    # Release all and verify subsequent request returns to primary replica
    lb.release(idx0)
    lb.release(idx1)
    lb.release(idx2)
    idx3, _, s3 = lb.acquire("ACME-100")
    assert idx3 == idx0
    assert not s3


def test_zipf_sampling_creates_heavy_hitter_skew() -> None:
    eids = [f"ACME-{100 + i:03d}" for i in range(48)]
    uni = sample_session_entities(eids, n_sessions=48, zipf_alpha=0.0, seed=42)
    assert len(set(uni)) == 48
    zipf = sample_session_entities(eids, n_sessions=96, zipf_alpha=1.2, seed=42)
    top3 = sum(c for _, c in Counter(zipf).most_common(3))
    assert top3 >= 35
