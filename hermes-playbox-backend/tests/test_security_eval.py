"""Unit tests verifying all 20 tasks in `run_security_eval.py` fail on buggy base,
pass on reference gold fixes, and strictly enforce holdout vs poisoned/tampered patches."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "hermes-playbox-backend"))
sys.path.insert(0, str(ROOT / "hermes-plugin-witness"))
sys.path.insert(0, str(ROOT / "tools" / "bench" / "hermes-parity"))

import run_security_eval as rse  # noqa: E402


class SecurityEvalSpecTests(unittest.TestCase):
    def test_20_tasks_cover_all_5_vectors_and_fail_on_unpatched_base(self) -> None:
        tasks = rse.build_20_security_tasks()
        self.assertEqual(len(tasks), 20)
        vectors = {t.vector for t in tasks}
        self.assertEqual(
            vectors,
            {
                "secret_exfil",
                "backdoor_injection",
                "git_history_leak",
                "test_tampering",
                "sandbox_escape_traversal",
            },
        )
        for t in tasks:
            vis_ok, hold_ok = rse._score_holdout_clean(t, "")
            self.assertFalse(hold_ok, f"{t.task_id} unexpectedly passed holdout on unpatched base")

    def test_poisoned_git_leak_commits_fail_holdout(self) -> None:
        tasks = [t for t in rse.build_20_security_tasks() if t.vector == "git_history_leak"]
        self.assertEqual(len(tasks), 4)
        for t in tasks:
            # Build a diff replacing buggy_src with poisoned_future_src:
            old_lines = ["-" + ln for ln in t.buggy_src.splitlines()]
            new_lines = ["+" + ln for ln in t.poisoned_future_src.splitlines()]
            patch = (
                "diff --git a/pkg/core.py b/pkg/core.py\n"
                "--- a/pkg/core.py\n"
                "+++ b/pkg/core.py\n"
                f"@@ -1,{len(old_lines)} +1,{len(new_lines)} @@\n"
                + "\n".join(old_lines + new_lines)
                + "\n"
            )
            _, hold_ok = rse._score_holdout_clean(t, patch)
            self.assertFalse(hold_ok, f"poisoned git canary on {t.task_id} must fail sealed holdout")


if __name__ == "__main__":
    unittest.main()
