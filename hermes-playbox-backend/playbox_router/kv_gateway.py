"""Verified Parametric AtlasKV + Entity-Routed (`S-ROUTED`) gateway helper for `playbox-aiproxy`.

Bridges offline hybrid `Ed25519+ML-DSA-65` signed AtlasKV shards and entity-routed working-set
selection into `playbox_router.aiproxy` receipt summaries:
- Enforces `require_pq=True` verification before exposing any shard cluster to a room worker.
- Routes queries to the matched entity working set (or falls back to HiKVP top-k clusters)
  with zero untrusted/quarantined triples.
- Produces a compact canonical `kv_gateway` metadata block that binds shard digests and
  routing telemetry directly into `playbox.attested_gate_receipt.v1`.
"""
from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence


@dataclass(frozen=True)
class KVRouteDecision:
    entity: str | None
    selected_clusters: tuple[str, ...]
    injected_kv_slots: int
    total_tokens_encoded: int
    shard_tensors_sha256: str
    pq_verified: bool
    quarantined_dropped: int

    def to_receipt_dict(self) -> dict[str, Any]:
        return {
            "schema": "playbox.kv_gateway_route.v1",
            "entity": self.entity,
            "selected_clusters": list(self.selected_clusters),
            "injected_kv_slots": self.injected_kv_slots,
            "total_tokens_encoded": self.total_tokens_encoded,
            "shard_tensors_sha256": self.shard_tensors_sha256,
            "pq_verified": self.pq_verified,
            "quarantined_dropped": self.quarantined_dropped,
        }


class VerifiedKVGateway:
    """Room-scoped verified AtlasKV + entity-routed working-set selector."""

    def __init__(
        self,
        clusters: Sequence[Any],
        shard_meta: dict[str, Any],
        *,
        entity_aliases: dict[str, Sequence[str]] | None = None,
    ) -> None:
        self.clusters = list(clusters)
        self.shard_meta = dict(shard_meta)
        self.by_inter: dict[str, Any] = {c.inter_key: c for c in self.clusters}
        self.entity_aliases: dict[str, tuple[str, ...]] = {
            k: tuple(v) for k, v in (entity_aliases or {}).items()
        }
        self._route_count = 0
        self._slots_injected_total = 0

    @classmethod
    def from_signed_shard(
        cls,
        shard_dir: str | Path,
        *,
        expected_pubkey_hex: str | None = None,
        expected_pq_pubkey_hex: str | None = None,
        require_pq: bool = True,
        entity_aliases: dict[str, Sequence[str]] | None = None,
    ) -> "VerifiedKVGateway":
        from playbox_atlas.parametric import load_verified_kb_shard

        clusters, meta = load_verified_kb_shard(
            shard_dir,
            expected_pubkey_hex=expected_pubkey_hex,
            expected_pq_pubkey_hex=expected_pq_pubkey_hex,
            require_pq=require_pq,
        )
        return cls(clusters, meta, entity_aliases=entity_aliases)

    def resolve_entity(self, query: str) -> str | None:
        q_low = query.lower()
        for entity, aliases in self.entity_aliases.items():
            if entity.lower() in q_low:
                return entity
            for a in aliases:
                if a.lower() in q_low:
                    return entity
        for c in self.clusters:
            if c.inter_key != "__preamble__" and c.inter_key.lower() in q_low:
                return c.inter_key
        return None

    def route(self, query: str, *, top_clusters: int = 2) -> tuple[list[Any], KVRouteDecision]:
        matched_entity = self.resolve_entity(query)
        selected: list[Any] = []
        if "__preamble__" in self.by_inter:
            selected.append(self.by_inter["__preamble__"])

        if matched_entity and matched_entity in self.by_inter:
            selected.append(self.by_inter[matched_entity])
        else:
            q_toks = set(re.findall(r"[a-z0-9_./-]+", query.lower()))
            scored = []
            for c in self.clusters:
                if c.inter_key == "__preamble__":
                    continue
                c_toks = set(re.findall(r"[a-z0-9_./-]+", (c.inter_key + " " + c.text).lower()))
                scored.append((len(q_toks & c_toks), c))
            scored.sort(key=lambda x: x[0], reverse=True)
            for _, c in scored[:top_clusters]:
                selected.append(c)

        slots = sum(int(c.slot_count) for c in selected)
        toks = sum(int(c.token_count) for c in selected)
        self._route_count += 1
        self._slots_injected_total += slots
        decision = KVRouteDecision(
            entity=matched_entity,
            selected_clusters=tuple(c.inter_key for c in selected),
            injected_kv_slots=slots,
            total_tokens_encoded=toks,
            shard_tensors_sha256=str(self.shard_meta.get("tensors_sha256") or ""),
            pq_verified=bool(self.shard_meta.get("pq_verified", False)),
            quarantined_dropped=int(self.shard_meta.get("quarantined_dropped", 0)),
        )
        return selected, decision

    def receipt_summary(self) -> dict[str, Any]:
        return {
            "schema": "playbox.kv_gateway_summary.v1",
            "model_id": self.shard_meta.get("model_id", ""),
            "shard_tensors_sha256": self.shard_meta.get("tensors_sha256", ""),
            "pq_verified": bool(self.shard_meta.get("pq_verified", False)),
            "quarantined_dropped": int(self.shard_meta.get("quarantined_dropped", 0)),
            "routes_served": self._route_count,
            "total_kv_slots_injected": self._slots_injected_total,
        }
