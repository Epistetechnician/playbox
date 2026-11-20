"""Metered intelligence-per-cost orchestrator combining Playbox's proven research stack:

1. AtlasKV-inspired KG2KV + HiKVP structural brief (zero shell round trips, prefix-stable prompt).
2. Cheap parallel worker fan-out with strict per-worker and total USD budget ceilings.
3. Cross-candidate 3-way composition (reusing `challenge/compose.py`) when partial fixes complement each other.
4. Fresh-sandbox sealed verification gate (never trusting in-session output) with provenance receipt.
"""
from .aiproxy import (
    LEDGER_SCHEMA,
    AiproxyClient,
    AiproxyError,
    MintedWorkerToken,
    WorkerTokenLease,
    canonical_ledger_bytes,
    compute_ledger_sha256,
    load_admin_key,
    summarize_ledger_for_receipt,
    verify_ledger,
)
from .receipt import (
    RECEIPT_SCHEMA,
    build_signed_receipt,
    get_playbox_tool_version,
    load_or_create_signing_key,
    verify_receipt_ledger,
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
from .trajectory_gate import (
    CertifiedTrajectoryGate,
    GateDecisionV2,
    admit_trajectory_v2,
    strip_tests_from_patch,
)

__all__ = [
    "AiproxyClient",
    "AiproxyError",
    "BudgetCap",
    "CandidateResult",
    "CertifiedTrajectoryGate",
    "GateDecisionV2",
    "LEDGER_SCHEMA",
    "MintedWorkerToken",
    "RECEIPT_SCHEMA",
    "ReproGateResult",
    "RouteDecision",
    "WorkerTokenLease",
    "admit_trajectory_v2",
    "build_escalation_brief",
    "build_signed_receipt",
    "canonical_ledger_bytes",
    "compose_patches",
    "compute_ledger_sha256",
    "get_playbox_tool_version",
    "is_genuine_repro_failure",
    "load_admin_key",
    "load_or_create_signing_key",
    "run_cascade",
    "select_and_compose",
    "strip_tests_from_patch",
    "summarize_ledger_for_receipt",
    "verify_ledger",
    "verify_receipt_ledger",
    "verify_signed_receipt",
    "verify_single_commit_repo",
]
