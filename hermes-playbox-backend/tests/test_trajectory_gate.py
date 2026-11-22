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
    admit_trajectory_v3,
    run_cascade,
    select_and_compose,
    strip_tests_from_patch,
    strip_tests_from_patch_v3,
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


def test_strip_tests_from_patch_v3_blocks_hooks_and_renames() -> None:
    patch = (
        "diff --git a/src/core.py b/src/core.py\n"
        "--- a/src/core.py\n"
        "+++ b/src/core.py\n"
        "@@ -1 +1 @@\n"
        "-return 0\n"
        "+return 1\n"
        "diff --git a/pkg/conftest.py b/pkg/conftest.py\n"
        "--- a/pkg/conftest.py\n"
        "+++ b/pkg/conftest.py\n"
        "@@ -0,0 +1 @@\n"
        "+def pytest_collection_modifyitems(items): items.clear()\n"
        "diff --git a/tests/old_test.py b/src/renamed_module.py\n"
        "--- a/tests/old_test.py\n"
        "+++ b/src/renamed_module.py\n"
        "@@ -1 +1 @@\n"
        "-assert False\n"
        "+assert True\n"
        "diff --git a/src/fake.py b/sitecustomize.pth\n"
        "--- a/src/fake.py\n"
        "+++ b/sitecustomize.pth\n"
        "@@ -0,0 +1 @@\n"
        "+import os\n"
    )
    sanitized, stripped_tests, blocked_hooks = strip_tests_from_patch_v3(patch)
    assert "src/core.py" in sanitized
    assert "conftest.py" not in sanitized
    assert "sitecustomize.pth" not in sanitized
    assert "old_test.py" not in sanitized
    assert stripped_tests == ["tests/old_test.py"]
    assert blocked_hooks == ["pkg/conftest.py", "sitecustomize.pth"]


def test_admit_trajectory_v3_edge_cases() -> None:
    # 1. Malformed trace and missing submit_patch fail closed
    assert admit_trajectory_v3({"steps": "bad"}).reasons == ("malformed_trace",)
    assert admit_trajectory_v3({"steps": []}).reasons == ("no_submit",)

    # 2. py_compile-only check rejected under require_behavioral_check=True, allowed when False
    py_compile_trace = {
        "steps": [
            {"tool_calls": [{"function_name": "edit_file", "arguments": {"filepath": "src/core.py"}}], "observation": {"content": "ok"}},
            {"tool_calls": [{"function_name": "run_command", "arguments": {"command": "python -m py_compile src/core.py"}}], "observation": {"content": "ok"}},
            {"tool_calls": [{"function_name": "submit_patch", "arguments": {}}], "observation": {"content": "submitted"}},
        ]
    }
    dec_strict = admit_trajectory_v3(py_compile_trace, require_behavioral_check=True)
    assert dec_strict.admitted is False
    assert "unverified_after_last_edit" in dec_strict.reasons
    dec_relaxed = admit_trajectory_v3(py_compile_trace, require_behavioral_check=False)
    assert dec_relaxed.admitted is True

    # 3. Shell mutation (sed -i) after a passing test invalidates verification and flags hook tampering
    shell_tamper_trace = {
        "steps": [
            {"tool_calls": [{"function_name": "edit_file", "arguments": {"filepath": "src/core.py"}}], "observation": {"content": "ok"}},
            {"tool_calls": [{"function_name": "run_command", "arguments": {"command": "pytest -q"}}], "observation": {"content": "1 passed"}},
            {"tool_calls": [{"function_name": "run_command", "arguments": {"command": "sed -i 's/x/y/' conftest.py"}}], "observation": {"content": "ok"}},
            {"tool_calls": [{"function_name": "submit_patch", "arguments": {}}], "observation": {"content": "submitted"}},
        ]
    }
    dec_shell = admit_trajectory_v3(shell_tamper_trace)
    assert dec_shell.admitted is False
    assert "harness_hook_tampering" in dec_shell.reasons
    assert "unverified_after_last_edit" in dec_shell.reasons
    assert "conftest.py" in dec_shell.blocked_hooks

    # 4. Empty patch after test stripping is rejected even when trace ran pytest
    dec_empty = admit_trajectory_v3(_tdd_trace(), patch=TEST_ONLY_PATCH, strip_tests_at_broker=True)
    assert dec_empty.admitted is False
    assert "empty_patch_after_test_strip" in dec_empty.reasons

    # 5. Scratch file written without cleanup: sanitized when broker strips, rejected otherwise
    scratch_trace = {
        "steps": [
            {"tool_calls": [{"function_name": "edit_file", "arguments": {"filepath": "src/core.py"}}], "observation": {"content": "ok"}},
            {"tool_calls": [{"function_name": "write_file", "arguments": {"filepath": "_pbx_repro.py"}}], "observation": {"content": "ok"}},
            {"tool_calls": [{"function_name": "run_command", "arguments": {"command": "pytest -q"}}], "observation": {"content": "1 passed"}},
            {"tool_calls": [{"function_name": "submit_patch", "arguments": {}}], "observation": {"content": "submitted"}},
        ]
    }
    dec_scratch_ok = admit_trajectory_v3(scratch_trace, strip_scratch_at_broker=True)
    assert dec_scratch_ok.admitted is True
    assert dec_scratch_ok.sanitized_scratch == ("_pbx_repro.py",)
    dec_scratch_rej = admit_trajectory_v3(scratch_trace, strip_scratch_at_broker=False)
    assert dec_scratch_rej.admitted is False
    assert "scratch_pollution_in_patch" in dec_scratch_rej.reasons

