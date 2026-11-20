"""Unit tests for `playbox_router.trajectory_gate` and broker-side `strip_tests_at_broker`."""
from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "hermes-playbox-backend"))

from playbox_router import (  # noqa: E402
    BudgetCap,
    CandidateResult,
    CertifiedTrajectoryGate,
    admit_trajectory_v2,
    run_cascade,
    select_and_compose,
    strip_tests_from_patch,
)

MIXED_PATCH = (
    "diff --git a/src/core.py b/src/core.py\n"
    "--- a/src/core.py\n"
    "+++ b/src/core.py\n"
    "@@ -1 +1 @@\n"
    "-return 0\n"
    "+return 1\n"
    "diff --git a/tests/test_core.py b/tests/test_core.py\n"
    "--- a/tests/test_core.py\n"
    "+++ b/tests/test_core.py\n"
    "@@ -1 +1 @@\n"
    "-assert False\n"
    "+assert True\n"
)

TEST_ONLY_PATCH = (
    "diff --git a/tests/test_core.py b/tests/test_core.py\n"
    "--- a/tests/test_core.py\n"
    "+++ b/tests/test_core.py\n"
    "@@ -1 +1 @@\n"
    "-assert False\n"
    "+assert True\n"
)


def _tdd_trace() -> dict:
    return {
        "steps": [
            {
                "tool_calls": [{"function_name": "edit_file", "arguments": {"filepath": "src/core.py"}}],
                "observation": {"content": "ok"},
            },
            {
                "tool_calls": [{"function_name": "run_command", "arguments": {"command": "pytest -q"}}],
                "observation": {"content": "FAILED tests/test_core.py::test_x - AssertionError"},
            },
            {
                "tool_calls": [{"function_name": "edit_file", "arguments": {"filepath": "tests/test_core.py"}}],
                "observation": {"content": "ok"},
            },
            {
                "tool_calls": [{"function_name": "run_command", "arguments": {"command": "pytest -q"}}],
                "observation": {"content": "2 passed in 0.05s"},
            },
            {
                "tool_calls": [{"function_name": "submit_patch", "arguments": {}}],
                "observation": {"content": "submitted"},
            },
        ]
    }


def _unverified_trace() -> dict:
    return {
        "steps": [
            {
                "tool_calls": [{"function_name": "edit_file", "arguments": {"filepath": "src/core.py"}}],
                "observation": {"content": "ok"},
            },
            {
                "tool_calls": [{"function_name": "submit_patch", "arguments": {}}],
                "observation": {"content": "submitted"},
            },
        ]
    }


def test_strip_tests_from_patch() -> None:
    sanitized, stripped = strip_tests_from_patch(MIXED_PATCH)
    assert "src/core.py" in sanitized
    assert "tests/test_core.py" not in sanitized
    assert stripped == ["tests/test_core.py"]

    empty, stripped_only = strip_tests_from_patch(TEST_ONLY_PATCH)
    assert empty.strip() == ""
    assert stripped_only == ["tests/test_core.py"]


def test_certified_trajectory_gate_v2_tdd_vs_unverified() -> None:
    gate_v2 = CertifiedTrajectoryGate(strip_tests_at_broker=True)
    dec_v2 = gate_v2.admit(_tdd_trace())
    assert dec_v2.admitted is True
    assert dec_v2.reasons == ()
    assert dec_v2.sanitized_tests == ("tests/test_core.py",)

    gate_v1 = CertifiedTrajectoryGate(strip_tests_at_broker=False)
    dec_v1 = gate_v1.admit(_tdd_trace())
    assert dec_v1.admitted is False
    assert "test_edit_after_failure" in dec_v1.reasons

    dec_unv = admit_trajectory_v2(_unverified_trace(), strip_tests_at_broker=True)
    assert dec_unv.admitted is False
    assert "unverified_after_last_edit" in dec_unv.reasons


def test_router_select_and_cascade_strip_tests_at_broker(tmp_path: Path) -> None:
    seen_patches: list[str] = []

    def fake_verify(patch: str) -> tuple[bool, float, dict]:
        seen_patches.append(patch)
        return ("return 1" in patch), (1.0 if "return 1" in patch else 0.0), {}

    c_tamper = CandidateResult(
        worker_id="w_tamper",
        model="m1",
        patch=TEST_ONLY_PATCH,
        cost_usd=0.01,
        audit_verified=True,
    )
    c_tdd = CandidateResult(
        worker_id="w_tdd",
        model="m1",
        patch=MIXED_PATCH,
        cost_usd=0.02,
        audit_verified=True,
    )
    dec = select_and_compose(
        tmp_path,
        [c_tamper, c_tdd],
        fake_verify,
        BudgetCap(),
        strip_tests_at_broker=True,
    )
    assert dec.candidates_evaluated == 1
    assert dec.winner is not None
    assert dec.winner.worker_id == "w_tdd"
    assert dec.winner.passed is True
    assert "tests/test_core.py" not in dec.winner.patch
    assert len(seen_patches) == 1
    assert "tests/test_core.py" not in seen_patches[0]
    assert dec.provenance["strip_tests_at_broker"] is True
    assert dec.provenance["candidates"][0]["stripped_test_files"] == ["tests/test_core.py"]

    # Escalated test-only tampering patch must also be rejected when strip_tests_at_broker=True
    c_fail = CandidateResult(
        worker_id="w_fail",
        model="m1",
        patch="diff --git a/src/core.py b/src/core.py\n--- a/src/core.py\n+++ b/src/core.py\n@@ -1 +1 @@\n-return 0\n+return 2\n",
        cost_usd=0.01,
        audit_verified=True,
    )
    dec_esc = run_cascade(
        tmp_path,
        [c_fail],
        online_verify=fake_verify,
        budget=BudgetCap(),
        escalate_fn=lambda brief, rem: CandidateResult(
            worker_id="w_esc_tamper",
            model="m_frontier",
            patch=TEST_ONLY_PATCH,
            cost_usd=0.05,
            audit_verified=True,
        ),
        strip_tests_at_broker=True,
    )
    assert dec_esc.escalated is True
    assert dec_esc.escalation_won is False
    assert dec_esc.provenance["escalation_rejected"] == "unverified_audit_or_empty"
