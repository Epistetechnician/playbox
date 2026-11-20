"""Unit tests for `run_verified_eval.py` (checkpoint/resume, McNemar exact test, Wilson CI,
non-oracle critic signature isolation, and receipt `tool_version` binding)."""
from __future__ import annotations

import inspect
import sys
import tempfile
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "hermes-playbox-backend"))
sys.path.insert(0, str(ROOT / "tools" / "bench" / "hermes-parity"))

import run_verified_eval as rve  # noqa: E402
from playbox_router import build_signed_receipt, load_or_create_signing_key, verify_signed_receipt  # noqa: E402


class VerifiedEvalHarnessTests(unittest.TestCase):
    def test_mcnemar_and_wilson_ci(self) -> None:
        # 26/28 P2 vs 18/28 S where P2 solves 8 tasks S missed and S solves 0 tasks P2 missed:
        paired = [(True, True)] * 18 + [(True, False)] * 8 + [(False, False)] * 2
        res = rve.exact_mcnemar_test(paired)
        self.assertEqual(res["paired_n"], 28)
        self.assertEqual(res["p2_only_solved_b"], 8)
        self.assertEqual(res["s_only_solved_c"], 0)
        self.assertAlmostEqual(res["p_value_two_sided"], 2.0 / 256.0, places=6)
        self.assertTrue(res["significant_alpha_0_05_two_sided"])

        lo, hi = rve.wilson_95_ci(27, 28)
        self.assertGreater(lo, 0.80)
        self.assertLessEqual(hi, 1.0)

    def test_checkpoint_roundtrip_and_resume(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            ckpt = Path(td) / "checkpoint.jsonl"
            lock = threading.Lock()
            rve._append_checkpoint(ckpt, lock, {"status": "completed", "arm": "S", "inst": "django-11848", "resolved": True})
            rve._append_checkpoint(ckpt, lock, {"status": "completed", "arm": "P2", "inst": "django-11848", "resolved": True})
            loaded = rve._load_checkpoint(ckpt)
            self.assertEqual(set(loaded.keys()), {("S", "django-11848"), ("P2", "django-11848")})

    def test_non_oracle_critic_signature_has_no_holdout_access(self) -> None:
        sig = inspect.signature(rve.run_non_oracle_critic)
        forbidden = {"res", "holdout", "test_patch", "f2p", "p2p", "FAIL_TO_PASS", "PASS_TO_PASS"}
        self.assertTrue(forbidden.isdisjoint(set(sig.parameters.keys())))
        ok, msg, cost = rve.run_non_oracle_critic(
            inst="django-11848",
            problem_statement="fix date parse",
            repo_cache_dir=Path("/nonexistent"),
            patch_text="",
            online_gate_passed=False,
            critic_model="",
        )
        self.assertFalse(ok)
        self.assertEqual(cost, 0.0)
        self.assertIn("empty", msg.lower())

    def test_receipt_binds_tool_version_and_verifies(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            sk = load_or_create_signing_key(Path(td) / "k.ed25519")
            rcpt = build_signed_receipt(
                instance="django-11848",
                base_commit="f0adf3b9b7",
                runtime="openshell",
                git_history_isolated=True,
                host_audit_verified=True,
                signing_key=sk,
            )
            self.assertIn("tool_version", rcpt["payload"])
            self.assertIn("git_dirty", rcpt["payload"])
            ok, _ = verify_signed_receipt(rcpt)
            self.assertTrue(ok)


if __name__ == "__main__":
    unittest.main()
