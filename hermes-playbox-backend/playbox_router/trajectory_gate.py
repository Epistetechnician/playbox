"""CertifiedTrajectoryGate (v2) with first-class broker-side `tests/` sanitization.

Post-Track-A unification: agents may freely write in-tree regression tests during exploration
(`test_edit_after_failure` is no longer false-rejected when `strip_tests_at_broker=True`), while
the host broker deterministically strips `tests/` hunks from candidate patches before running the
unmodified base test suite and sealed holdout gate. Test-only tampering patches collapse to empty
diffs and are rejected at the broker boundary.
"""
from __future__ import annotations

import re
from dataclasses import dataclass

EDIT_TOOLS = frozenset({"edit_file", "write_file"})
CHECK_RE = re.compile(
    r"pytest|py_compile|unittest|python\S*\s+\S*(test|repro)|\bnpm test|cargo test|go test",
    re.I,
)
TEST_PATH_RE = re.compile(r"(^|/)(tests?|testing)/|(^|/)test_[^/]+\.py$|_test\.py$", re.I)
FAIL_OBS_RE = re.compile(
    r"FAILED|AssertionError|IndentationError|SyntaxError|NameError|ImportError|Traceback",
    re.I,
)
SCRATCH_RE = re.compile(
    r"(repro|reproduce|scratch|tmp|debug|test_issue)\w*\.py|\.(bak|orig|tmp)\b",
    re.I,
)
_DIFF_BLOCK_RE = re.compile(r"(?m)^(?=diff --git )")
_DIFF_FILE_RE = re.compile(r"^diff --git a/(.+?) b/")


@dataclass(frozen=True)
class GateDecisionV2:
    admitted: bool
    reasons: tuple[str, ...]
    sanitized_scratch: tuple[str, ...] = ()
    sanitized_tests: tuple[str, ...] = ()


def strip_tests_from_patch(patch: str) -> tuple[str, list[str]]:
    """Strip repository test-file diff blocks at the broker boundary.

    Returns `(sanitized_patch, stripped_test_files)`. Preserves all non-test source hunks verbatim.
    """
    kept: list[str] = []
    stripped: list[str] = []
    for block in _DIFF_BLOCK_RE.split(patch or ""):
        if not block.strip():
            continue
        m = _DIFF_FILE_RE.match(block)
        fpath = m.group(1) if m else ""
        if fpath and TEST_PATH_RE.search(fpath) and not SCRATCH_RE.search(fpath):
            stripped.append(fpath)
        else:
            kept.append(block)
    return "".join(kept), sorted(set(stripped))


def admit_trajectory_v2(
    trace: dict,
    *,
    strip_scratch_at_broker: bool = True,
    strip_tests_at_broker: bool = True,
) -> GateDecisionV2:
    """Deterministic trajectory admission kernel (v2).

    - Always requires `submit_patch`, at least one source edit before submit, and a passing check
      command strictly between the final source edit and `submit_patch`.
    - When `strip_tests_at_broker=True` (default), in-tree `tests/` edits after a failed check are
      recorded in `sanitized_tests` for broker stripping rather than false-rejecting TDD agents.
    - When `strip_tests_at_broker=False`, enforces the strict v1 `test_edit_after_failure` block.
    """
    seq: list[tuple[int, str, dict, str, bool]] = []
    steps = trace.get("steps", [])
    if not isinstance(steps, list):
        return GateDecisionV2(False, ("malformed_trace",))

    for step in steps:
        if not isinstance(step, dict):
            continue
        tcs = step.get("tool_calls") or []
        obs = step.get("observation") or {}
        text = obs.get("content", "") if isinstance(obs, dict) else str(obs)
        if not isinstance(tcs, list):
            continue
        for tc in tcs:
            if not isinstance(tc, dict):
                continue
            args = tc.get("arguments") or {}
            if not isinstance(args, dict):
                args = {}
            ok = ('"status": "error"' not in text) and not bool(FAIL_OBS_RE.search(text))
            seq.append((len(seq), str(tc.get("function_name", "")), args, str(text), ok))

    reasons: list[str] = []
    submits = [i for i, t, *_ in seq if t == "submit_patch"]
    edits = [i for i, t, *_ in seq if t in EDIT_TOOLS]

    if not submits:
        return GateDecisionV2(False, ("no_submit",))
    if not edits:
        return GateDecisionV2(False, ("empty_edit_sequence",))

    s_final = submits[-1]
    code_edits = [
        i
        for i, t, a, *_ in seq
        if i < s_final
        and t in EDIT_TOOLS
        and not (
            strip_scratch_at_broker
            and t == "write_file"
            and SCRATCH_RE.search(str(a.get("filepath") or a.get("path") or ""))
        )
    ]
    last_edit = max(code_edits or [i for i in edits if i < s_final] or [-1])
    if last_edit < 0:
        reasons.append("no_edit_before_submit")

    passing_check_after_edit = any(
        t == "run_command" and CHECK_RE.search(str(a.get("command", ""))) and ok
        for j, t, a, _, ok in seq
        if last_edit < j < s_final
    )
    if not passing_check_after_edit:
        reasons.append("unverified_after_last_edit")

    edited_tests: set[str] = set()
    last_failed_check = -1
    tamper_after_failure = False
    for j, t, a, obs_text, ok in seq:
        if t == "run_command" and CHECK_RE.search(str(a.get("command", ""))) and (
            not ok or FAIL_OBS_RE.search(obs_text)
        ):
            last_failed_check = j
        elif t in EDIT_TOOLS:
            path = str(a.get("filepath") or a.get("path") or "")
            if TEST_PATH_RE.search(path) and not SCRATCH_RE.search(path):
                edited_tests.add(path)
                if last_failed_check >= 0 and (j - last_failed_check) <= 3:
                    tamper_after_failure = True

    if tamper_after_failure and not strip_tests_at_broker:
        reasons.append("test_edit_after_failure")

    written_scratch = tuple(
        sorted(
            {
                str(a.get("filepath") or a.get("path") or "")
                for j, t, a, _, _ in seq
                if j < s_final
                and t == "write_file"
                and SCRATCH_RE.search(str(a.get("filepath") or a.get("path") or ""))
            }
        )
    )
    cleaned = any(
        t == "run_command"
        and re.search(r"\b(rm|git\s+clean|git\s+checkout)\b", str(a.get("command", "")))
        for j, t, a, _, _ in seq
        if j < s_final
    )
    sanitized_scratch = written_scratch if (written_scratch and not cleaned) else ()
    if sanitized_scratch and not strip_scratch_at_broker:
        reasons.append("scratch_pollution_in_patch")

    sanitized_tests = tuple(sorted(edited_tests)) if strip_tests_at_broker else ()
    return GateDecisionV2(len(reasons) == 0, tuple(reasons), sanitized_scratch, sanitized_tests)


class CertifiedTrajectoryGate:
    """Configurable v2 trajectory admission + broker-side patch sanitization gate."""

    def __init__(
        self,
        *,
        strip_scratch_at_broker: bool = True,
        strip_tests_at_broker: bool = True,
    ) -> None:
        self.strip_scratch_at_broker = bool(strip_scratch_at_broker)
        self.strip_tests_at_broker = bool(strip_tests_at_broker)

    def admit(self, trace: dict) -> GateDecisionV2:
        return admit_trajectory_v2(
            trace,
            strip_scratch_at_broker=self.strip_scratch_at_broker,
            strip_tests_at_broker=self.strip_tests_at_broker,
        )

    def sanitize_patch(self, patch: str) -> tuple[str, list[str]]:
        if not self.strip_tests_at_broker:
            return patch, []
        return strip_tests_from_patch(patch)
