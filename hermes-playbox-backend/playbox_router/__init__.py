"""Metered intelligence-per-cost orchestrator combining Playbox's proven research stack:

1. AtlasKV-inspired KG2KV + HiKVP structural brief (zero shell round trips, prefix-stable prompt).
2. Cheap parallel worker fan-out with strict per-worker and total USD budget ceilings.
3. Cross-candidate 3-way composition (reusing `challenge/compose.py`) when partial fixes complement each other.
4. Fresh-sandbox sealed verification gate (never trusting in-session output) with provenance receipt.
"""
from .receipt import (
    RECEIPT_SCHEMA,
    build_signed_receipt,
    load_or_create_signing_key,
    verify_signed_receipt,
    verify_single_commit_repo,
)
from .router import (
    BudgetCap,
    CandidateResult,
    ReproGateResult,
    RouteDecision,
    build_escalation_brief,
    compose_patches,
    is_genuine_repro_failure,
    run_cascade,
    select_and_compose,
)

__all__ = [
    "BudgetCap",
    "CandidateResult",
    "RECEIPT_SCHEMA",
    "ReproGateResult",
    "RouteDecision",
    "build_escalation_brief",
    "build_signed_receipt",
    "compose_patches",
    "is_genuine_repro_failure",
    "load_or_create_signing_key",
    "run_cascade",
    "select_and_compose",
    "verify_signed_receipt",
    "verify_single_commit_repo",
]
