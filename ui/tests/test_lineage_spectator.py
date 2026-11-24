"""Tests for ancestry-based lineage credit ledger and spectator report."""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from hub import board
from hub.lineage import compute_lineage_credits


def test_lineage_ancestry_credit_and_rejections():
    events = [
        {
            "seq": 1,
            "sender": "alice",
            "text": board.encode("artifact", id="p1", status="verified", score=1.5, pq_verified=True),
        },
        {
            "seq": 2,
            "sender": "mallory",
            "text": board.encode("artifact", id="p_bad", parent="p1", status="failed", summary="holdout mismatch"),
        },
        {
            "seq": 3,
            "sender": "bob",
            "text": board.encode("artifact", id="p2", parent="p1", status="verified", score=2.2, pq_verified=True),
        },
    ]
    report = compute_lineage_credits(events, decay=0.5)
    assert report["schema"] == "playbox.lineage_credits.v1"
    assert report["summary"]["promoted"] == 2
    assert report["summary"]["rejected"] == 1
    assert report["summary"]["pq_verified_receipts"] == 2

    by_author = {c["author"]: c for c in report["contributors"]}
    # p1 gives 1.0 to alice; p2 gives 2/3 to bob and 1/3 to alice (parent)
    assert "mallory" not in by_author
    assert abs(by_author["alice"]["credits"] - (1.0 + 1.0 / 3.0)) < 1e-3
    assert abs(by_author["bob"]["credits"] - (2.0 / 3.0)) < 1e-3
    assert by_author["alice"]["ancestry_assists"] == 1
    assert by_author["bob"]["direct_promotions"] == 1
