"""Ancestry-based lineage credit ledger and read-only frontier spectator model.

Computes credit directly from signed room history / board events and optional 5-gate
attested receipt bundles without any centralized leaderboard:
- Each promoted frontier node distributes geometric-decay credit along its `parent`
  ancestry chain (`decay ** depth`, default `decay=0.5`), normalized per promotion so
  every accepted step sums to 1.0 credit.
- Contributors keep their signed ancestry credit even if they leave the room.
- Rejected candidates remain visible on the spectator timeline with their rejection
  reason and receipt digest, earning 0 credit.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Iterable

from . import board


def _extract_promotions_from_events(events: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    """Extract frontier promotion/rejection records from room history / `::pbx` events."""
    records: list[dict[str, Any]] = []
    for ev in events:
        if not isinstance(ev, dict):
            continue
        sender = str(ev.get("sender") or ev.get("author") or "unknown")
        ts = ev.get("ts") or ev.get("timestamp") or 0
        seq = ev.get("seq")
        # Direct gate receipt event format
        if ev.get("kind") in ("promote", "reject", "gate_decision"):
            records.append({
                "id": str(ev.get("id") or ev.get("receipt_sha256") or f"seq-{seq}"),
                "parent": ev.get("parent") or ev.get("parent_id"),
                "author": sender,
                "verdict": "promote" if ev.get("verdict", ev.get("kind")) == "promote" else "reject",
                "score": ev.get("score"),
                "reason": str(ev.get("reason") or ""),
                "receipt_sha256": str(ev.get("receipt_sha256") or ""),
                "pq_verified": bool(ev.get("pq_verified", False)),
                "ts": ts,
            })
            continue
        # `::pbx artifact` / `::pbx metric` feed messages
        text = ev.get("text") or ev.get("body") or ""
        parsed = board.parse(text) if isinstance(text, str) else None
        if not parsed:
            continue
        kind, payload = parsed
        if kind == "artifact" and payload.get("status") in ("verified", "failed"):
            verdict = "promote" if payload.get("status") == "verified" else "reject"
            records.append({
                "id": str(payload.get("id") or payload.get("sha256") or f"seq-{seq}"),
                "parent": payload.get("parent") or payload.get("parent_id"),
                "author": sender,
                "verdict": verdict,
                "score": payload.get("score"),
                "reason": str(payload.get("summary") or payload.get("reason") or ""),
                "receipt_sha256": str(payload.get("receipt_sha256") or payload.get("sha256") or ""),
                "pq_verified": bool(payload.get("pq_verified", False)),
                "ts": ts,
            })
    return records


def _extract_promotions_from_bundles(bundles: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for b in bundles:
        if not isinstance(b, dict):
            continue
        payload = b.get("payload") if isinstance(b.get("payload"), dict) else b
        sig = b.get("signature") if isinstance(b.get("signature"), dict) else {}
        verdict_raw = str(payload.get("verdict") or ("promote" if payload.get("passed") else "reject")).lower()
        verdict = "promote" if verdict_raw in ("promote", "pass", "passed", "verified") else "reject"
        records.append({
            "id": str(payload.get("submission_id") or payload.get("instance") or payload.get("id") or "bundle"),
            "parent": payload.get("parent_id") or payload.get("parent"),
            "author": str(payload.get("author") or payload.get("agent") or payload.get("worker") or "unknown"),
            "verdict": verdict,
            "score": payload.get("score"),
            "reason": str(payload.get("reason") or ""),
            "receipt_sha256": str(b.get("receipt_sha256") or payload.get("receipt_sha256") or ""),
            "pq_verified": str(sig.get("alg") or "") == "Ed25519+ML-DSA-65",
            "ts": payload.get("ts") or payload.get("created_at") or 0,
        })
    return records


def compute_lineage_credits(
    events: Iterable[dict[str, Any]] = (),
    *,
    bundles: Iterable[dict[str, Any]] = (),
    decay: float = 0.5,
    max_depth: int = 16,
) -> dict[str, Any]:
    """Compute signed-ancestry credit splits and frontier timeline for a room."""
    if not (0.0 <= decay <= 1.0):
        raise ValueError("decay must be in [0.0, 1.0]")

    records = _extract_promotions_from_events(events) + _extract_promotions_from_bundles(bundles)
    by_id: dict[str, dict[str, Any]] = {}
    for rec in records:
        if rec["verdict"] == "promote":
            by_id[rec["id"]] = rec

    raw_credits: dict[str, float] = {}
    direct_promotions: dict[str, int] = {}
    ancestry_assists: dict[str, int] = {}
    promoted_count = 0
    rejected_count = 0
    pq_count = 0

    for rec in records:
        if rec["pq_verified"]:
            pq_count += 1
        if rec["verdict"] != "promote":
            rejected_count += 1
            continue
        promoted_count += 1
        direct_promotions[rec["author"]] = direct_promotions.get(rec["author"], 0) + 1

        # Walk parent ancestry chain (cycle-safe)
        chain: list[dict[str, Any]] = []
        seen: set[str] = set()
        cur: dict[str, Any] | None = rec
        while cur is not None and len(chain) < max_depth:
            cid = cur["id"]
            if cid in seen:
                break
            seen.add(cid)
            chain.append(cur)
            pid = cur.get("parent")
            cur = by_id.get(str(pid)) if pid else None

        weights = [decay ** depth for depth in range(len(chain))]
        total_w = sum(weights) or 1.0
        for depth, (node, w) in enumerate(zip(chain, weights)):
            author = node["author"]
            raw_credits[author] = raw_credits.get(author, 0.0) + (w / total_w)
            if depth > 0:
                ancestry_assists[author] = ancestry_assists.get(author, 0) + 1

    total_credit = sum(raw_credits.values()) or 1.0
    contributors = [
        {
            "author": author,
            "credits": round(score, 4),
            "share_pct": round(100.0 * score / total_credit, 2),
            "direct_promotions": direct_promotions.get(author, 0),
            "ancestry_assists": ancestry_assists.get(author, 0),
        }
        for author, score in sorted(raw_credits.items(), key=lambda kv: (-kv[1], kv[0]))
    ]

    return {
        "schema": "playbox.lineage_credits.v1",
        "decay": decay,
        "summary": {
            "total_submissions": len(records),
            "promoted": promoted_count,
            "rejected": rejected_count,
            "pq_verified_receipts": pq_count,
            "contributors_count": len(contributors),
        },
        "contributors": contributors,
        "frontier": records,
    }


def load_bundles_from_dir(bundle_dir: str | Path) -> list[dict[str, Any]]:
    p = Path(bundle_dir)
    if not p.is_dir():
        return []
    out: list[dict[str, Any]] = []
    for f in sorted(p.glob("*.json")):
        try:
            data = json.loads(f.read_text(encoding="utf-8"))
            if isinstance(data, dict):
                out.append(data)
        except (OSError, ValueError):
            continue
    return out
