"""The backend waits out the host's per-member task rate limit instead of showing it to the model.
Run with Hermes' python:  PYTHONPATH=~/.hermes/hermes-agent <hermes venv python> -m unittest discover -s hermes-playbox-backend/tests"""
import os
import sys
import threading
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import playbox_terminal as pt  # noqa: E402


class FakeMcp:
    def __init__(self, fail_times, msg="task refused: rate limit: at most 30 tasks per 60s"):
        self.fail, self.msg, self.calls, self.args = fail_times, msg, 0, []

    def call(self, name, args=None):
        self.calls += 1
        self.args.append(args)
        if self.calls <= self.fail:
            raise RuntimeError(self.msg)
        return {"stdout": "", "stderr": "", "exit_code": 0, "patch": "", "patch_bytes": 0}


def env(mcp):
    s = pt._Session.__new__(pt._Session)
    s.image, s.base, s.stats_path, s.mcp = "img", "main", "", mcp
    s.started = False
    s.edits, s.patch_sig, s.last_patch, s.last_truncated, s.lock = [], pt.hashlib.sha256(b"0").hexdigest(), "", False, threading.Lock()
    e = pt.PlayboxEnvironment.__new__(pt.PlayboxEnvironment)
    e.s, e.cwd, e.timeout = s, "/sandbox", 10
    return e


class RateRetry(unittest.TestCase):
    def setUp(self):
        self.step = pt.RATE_WAIT_STEP
        pt.RATE_WAIT_STEP = 0.01

    def tearDown(self):
        pt.RATE_WAIT_STEP = self.step

    def test_rate_limit_is_retried(self):
        m = FakeMcp(3)
        r = env(m).execute("true")
        self.assertEqual(m.calls, 4)
        self.assertNotIn("refused", r["output"])

    def test_other_refusals_are_shown_at_once(self):
        m = FakeMcp(5, msg="task refused: not authorized")
        r = env(m).execute("true")
        self.assertEqual(m.calls, 1)
        self.assertIn("playbox refused", r["output"])

    def test_gives_up_after_the_cap(self):
        old = pt.RATE_WAIT_MAX
        pt.RATE_WAIT_MAX = 0.05
        try:
            m = FakeMcp(10**6)
            r = env(m).execute("true")
        finally:
            pt.RATE_WAIT_MAX = old
        self.assertIn("playbox refused", r["output"])

    def test_warm_is_asked_for_by_default_and_can_be_turned_off(self):
        m = FakeMcp(0)
        env(m).execute("true")
        self.assertTrue(m.args[0]["warm"])
        os.environ["PLAYBOX_WARM"] = "0"
        try:
            m2 = FakeMcp(0)
            env(m2).execute("true")
        finally:
            del os.environ["PLAYBOX_WARM"]
        self.assertFalse(m2.args[0]["warm"])


class Keep(unittest.TestCase):
    """With keep, earlier edits are not re-sent; they are replayed only if the sandbox says it lost them."""

    def test_no_replay_while_the_sandbox_is_kept(self):
        m = FakeMcp(0)
        e = env(m)
        e.s.edits.append(("/sandbox", "SECRET_EDIT_TEXT"))
        e.execute("echo one")
        e.execute("echo two")
        for a in m.args:
            self.assertNotIn("SECRET_EDIT_TEXT", a["command"])
            self.assertTrue(a["keep"] and a["warm"])
        self.assertNotIn("PBX_LOST", m.args[0]["command"])      # no check before the first command
        self.assertIn("PBX_LOST", m.args[1]["command"])         # a check from the second on

    def test_a_lost_sandbox_is_recovered_by_replay(self):
        class Lost(FakeMcp):
            def call(self, name, args=None):
                self.calls += 1
                self.args.append(args)
                if "PBX_LOST___; exit" in args["command"] and self.calls == 2:
                    return {"stdout": "___PBX_LOST___\n", "stderr": "", "exit_code": 97, "patch": "", "patch_bytes": 0}
                return {"stdout": "", "stderr": "", "exit_code": 0, "patch": "", "patch_bytes": 0}

        m = Lost(0)
        e = env(m)
        e.execute("echo one")
        e.s.edits.append(("/sandbox", "EDIT_TO_REPLAY"))
        e.execute("echo two")
        self.assertEqual(m.calls, 3)
        self.assertNotIn("EDIT_TO_REPLAY", m.args[1]["command"])
        self.assertIn("EDIT_TO_REPLAY", m.args[2]["command"])

    def test_keep_off_replays_every_time(self):
        old = pt.KEEP
        pt.KEEP = False
        try:
            m = FakeMcp(0)
            e = env(m)
            e.s.edits.append(("/sandbox", "EDIT_TO_REPLAY"))
            e.execute("echo one")
        finally:
            pt.KEEP = old
        self.assertIn("EDIT_TO_REPLAY", m.args[0]["command"])
        self.assertFalse(m.args[0]["keep"])


if __name__ == "__main__":
    unittest.main()
