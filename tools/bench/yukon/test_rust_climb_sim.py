"""End-to-end offline test: rust_climb with the claude CLI, gcloud and VM scoring stubbed.
Checks that SIM records every attempt, round 2's advisor prompt contains round 1's memory,
mispredictions are flagged, and a fresh run resumes from the persisted memory.
Run: python3 -m unittest tools/bench/yukon/test_rust_climb_sim.py
"""
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import rust_climb as rc
from sim import Sim


class Stub:
    def __init__(self):
        self.advisor_prompts = []
        self.round = 0

    def claude(self, prompt, model, cwd, tools, budget, effort="low", allowed=None):
        if tools == "Read,Grep,Glob":  # advisor
            self.advisor_prompts.append(prompt)
            self.round += 1
            return json.dumps([
                {"root_cause": "Redundant uncompute", "hypothesis": f"r{self.round} drop uncompute", "predicted_score": 880000000},
                {"root_cause": "ancilla pressure", "hypothesis": f"r{self.round} reuse ancilla", "predicted_score": 870000000},
                "plain string hypothesis",
                {"root_cause": "x", "hypothesis": "never scores", "predicted_score": 860000000},
            ])
        (Path(cwd) / "src/point_add/edit.rs").write_text(f"// {prompt[-40:]}\n")  # worker makes an edit
        return "did it"

    def sh(self, args, **kw):
        class R:
            stdout = ""
            stderr = ""
            returncode = 0
        r = R()
        if args[0] == "diff":
            r.stdout = "+x\n"
        return r

    def score(self, cands):
        by = {"w0": 888000000, "w1": 800000000, "w2": 890000000, "w3": None}  # w1 wins, w0 mispredicted, w3 fails
        for c in cands:
            s = by[c["id"][c["id"].index("w"):]]
            c["result"] = {"exit": 0 if s else 124, "score": s, **({} if s else {"reason": "benchmark timed out (1500s)"})}
        return cands


class E2E(unittest.TestCase):
    def test_two_rounds_and_resume(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            (t / "base/src/point_add").mkdir(parents=True)
            (t / "base/src/point_add/a.rs").write_text("//a\n")
            (t / "base/README.md").write_text("r")
            st = Stub()
            rc.BASE, rc.OUT, rc.ROUNDS, rc.N, rc.CAP = t / "base", t / "out", 2, 4, 99
            rc.claude, rc.sh, rc.score = st.claude, st.sh, st.score
            rc.main()

            sim = Sim(t / "out/sim.jsonl")
            self.assertEqual(len(sim.attempts), 8)  # every attempt recorded, failures included
            verdicts = [a["verdict"] for a in sim.attempts[:4]]
            self.assertEqual(verdicts, ["mispredicted", "improved", "no-gain", "failed"])
            self.assertIn("redundant uncompute", sim.entries())  # root cause normalized and grouped
            self.assertIn("timed out", sim.attempts[3]["rationale"])  # failure reason reaches the memory
            self.assertEqual(len(sim.entries()["redundant uncompute"]), 2)  # r1 + r2 under one entry
            self.assertEqual(st.advisor_prompts[0].count("ROOT CAUSE"), 0)
            p2 = st.advisor_prompts[1]  # round 2 sees round 1's memory
            self.assertIn("ROOT CAUSE: redundant uncompute", p2)
            self.assertIn("[mispredicted]", p2)
            self.assertIn("[failed]", p2)

            self.assertIn("ROOT CAUSE", Sim(t / "out/sim.jsonl").render())  # a later run resumes from disk
            self.assertEqual((t / "base/src/point_add/a.rs").read_text(), "//a\n")  # BASE untouched
            self.assertTrue((t / "out/best_point_add/edit.rs").exists())
            self.assertFalse((t / "out/.climb.lock").exists())

            # Resume: round numbering continues at 3, and best=800M is preserved (850M must not overwrite best_point_add)
            marker = t / "out/best_point_add/edit.rs"
            prev_best_content = marker.read_text()

            def score_850m(cands):
                for c in cands:
                    c["result"] = {"exit": 0, "score": 850000000}
                return cands

            rc.score = score_850m
            rc.ROUNDS = 1
            rc.main()
            sim2 = Sim(t / "out/sim.jsonl")
            self.assertEqual(len(sim2.attempts), 12)
            self.assertEqual(sim2.attempts[-1]["prov"]["round"], 3)
            self.assertEqual(sim2.attempts[-1]["baseline"], 800000000)
            self.assertEqual(sim2.attempts[-1]["verdict"], "no-gain")  # 850M > recovered best 800M, predicted=860M > 800M
            self.assertEqual(marker.read_text(), prev_best_content)


class SimEdgeCases(unittest.TestCase):
    def test_nan_inf_ties_and_corrupt_jsonl(self):
        with tempfile.TemporaryDirectory() as t:
            p = Path(t) / "sim.jsonl"
            sim = Sim(p)
            # NaN and inf must sanitize to None and never write invalid JSON
            a1 = sim.record("nan test", "edit1", "why", float("nan"), float("inf"), 100)
            self.assertEqual(a1["verdict"], "failed")
            self.assertIsNone(a1["predicted"])
            self.assertIsNone(a1["observed"])

            # Tie (observed == baseline)
            a2 = sim.record("tie", "edit2", "why", None, 100, 100)
            self.assertEqual(a2["verdict"], "no-gain")
            a3 = sim.record("tie mispred", "edit3", "why", 90, 100, 100)
            self.assertEqual(a3["verdict"], "mispredicted")

            # Append truncated/corrupt line as if process crashed mid-write
            with p.open("a") as f:
                f.write('{"root_cause": "truncated\n')
            reloaded = Sim(p)
            self.assertEqual(len(reloaded.attempts), 3)


class HardeningTests(unittest.TestCase):
    def test_score_and_result_parsing(self):
        self.assertEqual(rc.parse_score_value(12345), 12345)
        self.assertEqual(rc.parse_score_value("800000000.0"), 800000000)
        for bad in (None, True, False, 0, -1, "0", "-5", "nan", "inf", "-inf", "null", "err", float("nan"), float("inf")):
            self.assertIsNone(rc.parse_score_value(bad), f"expected None for {bad!r}")

        parsed = rc.parse_result_lines(
            "r1w0 0 800000000\n"
            "r1w1 124\n"
            "r1w2 0 nan\n"
            "r1w3 bad_exit 100\n"
            "noise line\n"
        )
        self.assertEqual(parsed["r1w0"], {"exit": 0, "score": 800000000})
        self.assertEqual(parsed["r1w1"], {"exit": 124, "score": None})
        self.assertEqual(parsed["r1w2"], {"exit": 0, "score": None})
        self.assertEqual(parsed["r1w3"], {"exit": None, "score": 100})
        self.assertNotIn("noise", parsed)

    def test_lock_reclaims_stale_and_blocks_live(self):
        with tempfile.TemporaryDirectory() as t:
            out = Path(t)
            lock = out / ".climb.lock"
            # Dead PID should be reclaimed
            lock.write_text("999999\n")
            with rc.climb_lock(out):
                self.assertEqual(int(lock.read_text().strip()), os.getpid())
            self.assertFalse(lock.exists())

            # Live PID (current process) should block
            lock.write_text(f"{os.getpid()}\n")
            with self.assertRaises(RuntimeError):
                with rc.climb_lock(out):
                    pass

    def test_save_best_dir_removes_stale_files(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            src1, src2, dst = t / "s1", t / "s2", t / "best"
            src1.mkdir()
            src2.mkdir()
            (src1 / "old.rs").write_text("old")
            (src1 / "keep.rs").write_text("v1")
            (src2 / "keep.rs").write_text("v2")
            rc.save_best_dir(src1, dst)
            self.assertTrue((dst / "old.rs").exists())
            rc.save_best_dir(src2, dst)
            self.assertFalse((dst / "old.rs").exists())
            self.assertEqual((dst / "keep.rs").read_text(), "v2")

    def test_worker_crash_does_not_drop_other_candidates(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            (t / "base/src/point_add").mkdir(parents=True)
            (t / "base/src/point_add/a.rs").write_text("//a\n")
            (t / "base/README.md").write_text("r")
            st = Stub()

            def flaky_claude(prompt, model, cwd, tools, budget, effort="low", allowed=None):
                if tools != "Read,Grep,Glob" and Path(cwd).name.endswith("w0"):
                    raise RuntimeError("simulated worker crash")
                return st.claude(prompt, model, cwd, tools, budget, effort, allowed)

            orig = (rc.BASE, rc.OUT, rc.ROUNDS, rc.N, rc.CAP, rc.spent, rc.claude, rc.sh, rc.score)
            try:
                rc.BASE, rc.OUT, rc.ROUNDS, rc.N, rc.CAP, rc.spent = t / "base", t / "out", 1, 2, 99, 0.0
                rc.claude, rc.sh, rc.score = flaky_claude, st.sh, st.score
                rc.main()
                sim = Sim(t / "out/sim.jsonl")
                self.assertEqual(len(sim.attempts), 1)
                self.assertEqual(sim.attempts[0]["prov"]["run"], "r1w1")
            finally:
                rc.BASE, rc.OUT, rc.ROUNDS, rc.N, rc.CAP, rc.spent, rc.claude, rc.sh, rc.score = orig


if __name__ == "__main__":
    unittest.main()
