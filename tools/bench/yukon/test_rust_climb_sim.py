"""End-to-end offline test: rust_climb with the claude CLI, gcloud and VM scoring stubbed.
Checks that SIM records every attempt, round 2's advisor prompt contains round 1's memory,
mispredictions are flagged, and a fresh run resumes from the persisted memory.
Run: python3 -m unittest tools/bench/yukon/test_rust_climb_sim.py
"""
import json, sys, tempfile, unittest
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


if __name__ == "__main__":
    unittest.main()
