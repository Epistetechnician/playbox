"""Log-Structured Delta-Framed Entity KV + Auto-Crossover Hierarchical Router.

Solves two production failure modes of prefix-cached entity serving:
1. Mid-prefix mutation invalidation: live updates to an account/repo/ticket
   rewrite bytes inside the dossier, invalidating vLLM's block-aligned prefix
   cache. ``DeltaFramedEntity`` freezes an immutable base snapshot and appends
   monotonic ``[DELTA #k]`` frames at the tail, preserving >90% prefix reuse
   until cost-aware compaction triggers.
2. The >=48K KV capacity wall (Section 8 crossover law): entities above the
   working-set threshold automatically switch from ``FULL-KV`` to
   ``HYBRID-HIER-KV`` — pinning a compact structured entity skeleton + live
   delta log in KV while retrieving only top-k relevant cold archive chunks.
"""
from __future__ import annotations

import re
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Sequence, Tuple

from playbox_steer.kv_sota_bench import BM25ChunkRetriever

CHARS_PER_TOKEN = 4

DELTA_OVERRIDE_RULE = (
    "AUTHORITATIVE STATE RULE: The BASE SNAPSHOT below is immutable. "
    "Any [DELTA #seq] frames appended after it represent newer live updates and "
    "STRICTLY OVERRIDE earlier values in the base snapshot or lower-numbered deltas."
)


def estimate_tokens(text: str) -> int:
    return max(1, (len(text) + CHARS_PER_TOKEN - 1) // CHARS_PER_TOKEN)


def prefix_preserved_ratio(old_prefix: str, new_prefix: str) -> float:
    """Fraction of ``old_prefix`` bytes preserved identically from byte 0."""
    if not old_prefix:
        return 1.0 if not new_prefix else 0.0
    limit = min(len(old_prefix), len(new_prefix))
    same = 0
    while same < limit and old_prefix[same] == new_prefix[same]:
        same += 1
    return same / len(old_prefix)


def extract_entity_skeleton(
    entity_id: str,
    name: str,
    dossier: str,
    max_skeleton_chars: int = 10000,
) -> Tuple[str, List[Tuple[str, str]]]:
    """Build a compact structured skeleton (~2.5K tokens) + cold archive chunks.

    Preserves all structured key-value header/summary lines and section markers
    in the skeleton, while slicing the full dossier into overlapping chunks for
    targeted per-entity retrieval under ``HYBRID-HIER-KV``.
    """
    lines = dossier.splitlines()
    header_lines: List[str] = []
    body_lines: List[str] = []
    key_kv_re = re.compile(r"^(===|[A-Z][A-Za-z0-9 _/\-]{1,36}:\s+\S)")

    for i, ln in enumerate(lines):
        if i < 20 or key_kv_re.match(ln.strip()):
            header_lines.append(ln)
        else:
            body_lines.append(ln)

    # Chunk the full dossier into ~1,600-char (~400-token) archive segments
    chunk_chars = 1600
    chunks: List[Tuple[str, str]] = []
    buf: List[str] = []
    buf_len = 0
    c_idx = 0
    for ln in lines:
        buf.append(ln)
        buf_len += len(ln) + 1
        if buf_len >= chunk_chars:
            chunks.append((f"{entity_id}:chunk_{c_idx:04d}", "\n".join(buf)))
            c_idx += 1
            buf = []
            buf_len = 0
    if buf:
        chunks.append((f"{entity_id}:chunk_{c_idx:04d}", "\n".join(buf)))

    skeleton_parts = [
        f"=== ENTITY SKELETON (HIERARCHICAL KV): {name} ({entity_id}) ===",
        f"Archive Chunks Indexed: {len(chunks)} (~{estimate_tokens(dossier):,} total tokens)",
    ]
    used = sum(len(p) + 1 for p in skeleton_parts)
    for ln in header_lines:
        if used + len(ln) + 1 > max_skeleton_chars:
            break
        skeleton_parts.append(ln)
        used += len(ln) + 1

    # Include tail excerpt of recent logs if space remains
    if body_lines and used < max_skeleton_chars - 400:
        skeleton_parts.append("--- Recent Tail Summary ---")
        tail_budget = max_skeleton_chars - used - 40
        tail_picked: List[str] = []
        t_used = 0
        for ln in reversed(body_lines):
            if t_used + len(ln) + 1 > tail_budget:
                break
            tail_picked.append(ln)
            t_used += len(ln) + 1
        skeleton_parts.extend(reversed(tail_picked))

    return "\n".join(skeleton_parts), chunks


@dataclass
class DeltaFrame:
    seq: int
    field: str
    value: str
    timestamp: str
    note: str = ""
    supersedes: Optional[str] = None

    def format_line(self) -> str:
        sup = f" (supersedes: {self.supersedes})" if self.supersedes is not None else ""
        note_s = f" | {self.note}" if self.note else ""
        return f"[DELTA #{self.seq:03d} | {self.timestamp}] {self.field}: {self.value}{sup}{note_s}"


@dataclass
class CompactionEvent:
    entity_id: str
    seq_at_compact: int
    deltas_merged: int
    stale_overwrites: int
    delta_ratio: float
    reason: str


class DeltaFramedEntity:
    """LSM-style immutable base prefix + append-only monotonic delta frames."""

    def __init__(
        self,
        entity_id: str,
        name: str,
        base_dossier: str,
        aliases: Optional[List[str]] = None,
        compact_delta_ratio: float = 0.25,
        compact_max_stale: int = 8,
        skeleton_max_chars: int = 10000,
    ) -> None:
        self.entity_id = entity_id
        self.name = name
        self.aliases = list(aliases or [])
        self.base_snapshot = base_dossier
        self.compact_delta_ratio = float(compact_delta_ratio)
        self.compact_max_stale = int(compact_max_stale)
        self.skeleton_max_chars = int(skeleton_max_chars)

        self.delta_frames: List[DeltaFrame] = []
        self.active_fields: Dict[str, str] = {}
        self.seq: int = 0
        self.stale_overwrites: int = 0
        self.compaction_count: int = 0
        self.compaction_history: List[CompactionEvent] = []

        self.skeleton_base, self.cold_chunks = extract_entity_skeleton(
            entity_id, name, base_dossier, max_skeleton_chars=self.skeleton_max_chars
        )
        self._chunk_retriever: Optional[Callable[[str], Tuple[List[str], Dict[str, float]]]] = None

    def _get_entity_retriever(self, top_k: int = 4) -> Callable[[str], Tuple[List[str], Dict[str, float]]]:
        if self._chunk_retriever is None:
            self._chunk_retriever = BM25ChunkRetriever(self.cold_chunks, top_k=top_k)
        return self._chunk_retriever

    def render_delta_block(self) -> str:
        if not self.delta_frames:
            return ""
        lines = ["=== LIVE DELTA LOG (AUTHORITATIVE OVERRIDES) ==="]
        lines.extend(f.format_line() for f in self.delta_frames)
        return "\n".join(lines)

    def render_full_prefix(self, system_preamble: str = "") -> str:
        """Render byte-stable full KV prefix: immutable base + appended deltas."""
        parts: List[str] = []
        if system_preamble:
            parts.append(system_preamble)
        parts.append(DELTA_OVERRIDE_RULE)
        parts.append(f"=== ENTITY DOSSIER: {self.name} ===\n{self.base_snapshot}")
        delta_blk = self.render_delta_block()
        if delta_blk:
            parts.append(delta_blk)
        return "\n\n".join(parts)

    def render_hierarchical_prefix(self, system_preamble: str = "") -> str:
        """Render compact ~2.5K skeleton + appended deltas for HYBRID-HIER-KV."""
        parts: List[str] = []
        if system_preamble:
            parts.append(system_preamble)
        parts.append(DELTA_OVERRIDE_RULE)
        parts.append(self.skeleton_base)
        delta_blk = self.render_delta_block()
        if delta_blk:
            parts.append(delta_blk)
        return "\n\n".join(parts)

    def retrieve_cold_chunks(self, query: str, top_k: int = 4) -> List[str]:
        if not self.cold_chunks:
            return []
        retriever = self._get_entity_retriever(top_k=top_k)
        docs, _ = retriever(query)
        return docs[:top_k]

    def total_estimated_tokens(self) -> int:
        return estimate_tokens(self.base_snapshot) + estimate_tokens(self.render_delta_block())

    def delta_ratio(self) -> float:
        delta_len = len(self.render_delta_block())
        return delta_len / max(1, len(self.base_snapshot))

    def append_delta(
        self,
        field_name: str,
        value: str,
        note: str = "",
        timestamp: Optional[str] = None,
        auto_compact: bool = True,
    ) -> Dict[str, Any]:
        """Append a live mutation without invalidating base_snapshot bytes."""
        old_prefix = self.render_full_prefix()
        prev_val = self.active_fields.get(field_name)
        if prev_val is not None:
            self.stale_overwrites += 1

        self.seq += 1
        ts = timestamp or time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        frame = DeltaFrame(
            seq=self.seq,
            field=field_name,
            value=value,
            timestamp=ts,
            note=note,
            supersedes=prev_val,
        )
        self.delta_frames.append(frame)
        self.active_fields[field_name] = value

        compacted = False
        compact_reason = ""
        if auto_compact:
            ev = self.maybe_compact()
            if ev is not None:
                compacted = True
                compact_reason = ev.reason

        new_prefix = self.render_full_prefix()
        return {
            "entity_id": self.entity_id,
            "seq": self.seq,
            "delta_count": len(self.delta_frames),
            "stale_overwrites": self.stale_overwrites,
            "delta_ratio": round(self.delta_ratio(), 4),
            "compacted": compacted,
            "compact_reason": compact_reason,
            "prefix_preserved_ratio": round(prefix_preserved_ratio(old_prefix, new_prefix), 4),
        }

    def maybe_compact(self, force: bool = False) -> Optional[CompactionEvent]:
        """Merge accumulated deltas into a fresh base snapshot when amortized cost warrants it."""
        if not self.delta_frames and not force:
            return None
        ratio = self.delta_ratio()
        reason = ""
        if force:
            reason = "forced"
        elif ratio >= self.compact_delta_ratio:
            reason = f"delta_ratio({ratio:.3f}>={self.compact_delta_ratio})"
        elif self.stale_overwrites >= self.compact_max_stale:
            reason = f"stale_overwrites({self.stale_overwrites}>={self.compact_max_stale})"
        else:
            return None

        # Materialize authoritative active state into the base snapshot (regex replace if present, else append)
        updated = self.base_snapshot
        merged_lines: List[str] = []
        for k, v in self.active_fields.items():
            pat = re.compile(rf"^({re.escape(k)}\s*:\s*).*$", flags=re.MULTILINE)
            if pat.search(updated):
                updated = pat.sub(rf"\g<1>{v}", updated, count=1)
            else:
                merged_lines.append(f"{k}: {v}")
        if merged_lines:
            updated = updated.rstrip() + "\n" + "\n".join(merged_lines) + "\n"

        ev = CompactionEvent(
            entity_id=self.entity_id,
            seq_at_compact=self.seq,
            deltas_merged=len(self.delta_frames),
            stale_overwrites=self.stale_overwrites,
            delta_ratio=round(ratio, 4),
            reason=reason,
        )
        self.base_snapshot = updated
        self.delta_frames.clear()
        self.stale_overwrites = 0
        self.compaction_count += 1
        self.compaction_history.append(ev)
        self.skeleton_base, self.cold_chunks = extract_entity_skeleton(
            self.entity_id, self.name, self.base_snapshot, max_skeleton_chars=self.skeleton_max_chars
        )
        self._chunk_retriever = None
        return ev


@dataclass
class TierDecision:
    tier: str  # "FULL-KV" | "HYBRID-HIER-KV" | "CHUNKED-RAG"
    estimated_tokens: int
    reason: str


@dataclass
class AutoCrossoverPolicy:
    """Automatic serving-tier classifier grounded in the measured 4K->64K crossover law.

    Default thresholds reflect Section 8 empirical measurements on A100/H100:
    - L <= 13,200 tokens (L*_CONV): FULL-KV dominates Conv-RAG on speed and accuracy.
    - 13,200 < L < 48,000 tokens: FULL-KV if accuracy-prioritized and <= hybrid_switch_tokens,
      else HYBRID-HIER-KV (compact skeleton + live delta log in KV + cold chunk RAG).
    - L >= 48,000 tokens: active-KV capacity wall -> always HYBRID-HIER-KV (never full 48K+ KV).
    """

    l_full_max_tokens: int = 13200
    l_hybrid_switch_tokens: int = 24000
    l_wall_tokens: int = 48000
    prefer_accuracy: bool = True

    def classify(self, entity: Optional[DeltaFramedEntity]) -> TierDecision:
        if entity is None:
            return TierDecision(tier="CHUNKED-RAG", estimated_tokens=0, reason="no_entity_locality")
        est = entity.total_estimated_tokens()
        if est >= self.l_wall_tokens:
            return TierDecision(
                tier="HYBRID-HIER-KV",
                estimated_tokens=est,
                reason=f"above_kv_wall({est}>={self.l_wall_tokens})",
            )
        if est <= self.l_full_max_tokens:
            return TierDecision(
                tier="FULL-KV",
                estimated_tokens=est,
                reason=f"below_crossover({est}<={self.l_full_max_tokens})",
            )
        if self.prefer_accuracy and est <= self.l_hybrid_switch_tokens:
            return TierDecision(
                tier="FULL-KV",
                estimated_tokens=est,
                reason=f"accuracy_zone({est}<={self.l_hybrid_switch_tokens})",
            )
        return TierDecision(
            tier="HYBRID-HIER-KV",
            estimated_tokens=est,
            reason=f"hybrid_zone({est}>{self.l_full_max_tokens})",
        )
