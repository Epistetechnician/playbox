"""The E4 search loop on a synthetic landscape, with a fake lineage and with the real Rust one."""
import os
import random
import shutil
import tempfile
from pathlib import Path

import pytest

from playbox_orchestrator.steer_search import Config, Result, Space, neighbors, patch_between, search

ROOT = Path(__file__).resolve().parents[2]
BINARY = ROOT / "target/debug/playbox-mcp"


def landscape(c: Config) -> float:
    """True effect: best near layer 6, resid_post, strength 0.3; zero strength does nothing."""
    if c.coef == 0:
        return 0.0
    base = 1.0 - 0.12 * abs(c.layer - 6) - 1.5 * abs(c.coef - 0.3)
    return max(0.0, base * (1.0 if c.site == "resid_post" else 0.6))


def evaluator(noise, n, seed, overfit=lambda c: 0.0):
    def run(c: Config) -> Result:
        rng = random.Random(f"{seed}-{c.key()}")
        return Result([landscape(c) + overfit(c) + rng.gauss(0, noise) for _ in range(n)], kl=0.1 + 0.8 * c.coef)
    return run


class FakeLineage:
    """Stands in for the Rust check: significance from the last variant's scores against its parent's."""
    def __init__(self):
        self.signed = {}

    def sign(self, parent, patch, scores, cost):
        vid = f"v{len(self.signed)}"
        self.signed[vid] = {"parent": parent, "patch": patch, "scores": scores, "cost": cost}
        return vid, vid

    def check(self, chain):
        import statistics
        last, prev = self.signed[chain[-1][0]], self.signed[chain[-2][0]] if len(chain) > 1 else None
        if not prev:
            return {}
        a, b = last["scores"], prev["scores"]
        se = (statistics.variance(a) / len(a) + statistics.variance(b) / len(b)) ** 0.5
        d = statistics.fmean(a) - statistics.fmean(b)
        return {"improvement": {"delta": d, "lower_bound": d - 1.645 * se, "significant": d - 1.645 * se > 0}}


def run(dev, hold, **kw):
    return search(root=Config("resid_post", 3, 0.0), dev=dev, holdout=hold, lineage=FakeLineage(), seed=1, **kw)


def test_the_search_climbs_and_every_accepted_step_is_supported_on_the_holdout():
    r = run(evaluator(0.05, 24, "dev"), evaluator(0.05, 32, "hold"), rounds=8, width=4)
    assert landscape(r.best) > landscape(r.root) + 0.3
    accepted = [s for s in r.steps if s.decision == "accepted"]
    assert accepted and all(s.lower_bound > 0 for s in accepted)
    assert [c.key() for c in r.configs][0] == r.root.key() and r.configs[-1] == r.best
    assert len(r.chain) == len(accepted) + 1


def test_a_candidate_that_only_looks_good_on_dev_is_stopped_by_the_holdout():
    """Dev rewards a spurious bump at layer 9; the holdout, which the searcher cannot see, does not."""
    bump = lambda c: 2.0 if (c.layer == 9 and c.coef > 0) else 0.0  # noqa: E731
    r = run(evaluator(0.05, 24, "dev", overfit=bump), evaluator(0.05, 32, "hold"), rounds=10, width=4)
    assert r.best.layer != 9
    rejected = [s for s in r.steps if s.config.layer == 9 and s.decision.startswith("rejected: gain")]
    assert all(s.holdout_mean is not None and s.holdout_mean < s.dev_mean for s in rejected)


def test_the_kl_cap_blocks_strong_edits_on_dev_and_on_the_holdout():
    r = run(evaluator(0.02, 24, "dev"), evaluator(0.02, 32, "hold"), rounds=8, width=4, kl_cap=0.35)
    assert r.best.coef <= (0.35 - 0.1) / 0.8 + 1e-9
    assert any("KL over the cap" in s.decision for s in r.steps)
    # a holdout that disagrees about KL is also refused
    dev = evaluator(0.02, 24, "dev")
    hold = lambda c: Result(evaluator(0.02, 32, "hold")(c).scores, kl=5.0)  # noqa: E731
    r2 = run(dev, hold, rounds=4, width=3)
    assert r2.best.key() == r2.root.key() and any("on the holdout" in s.decision for s in r2.steps)


def test_the_holdout_budget_is_a_hard_limit_and_is_recorded_as_cost():
    counted = {"n": 0}
    base = evaluator(0.05, 32, "hold")

    def hold(c):
        counted["n"] += 1
        return base(c)

    lin = FakeLineage()
    r = search(root=Config("resid_post", 3, 0.0), dev=evaluator(0.05, 24, "dev"), holdout=hold, lineage=lin, seed=2, rounds=20, width=4, max_holdout_queries=3)
    assert counted["n"] == r.holdout_queries <= 3
    assert any("budget" in s.decision for s in r.steps)
    assert max(v["cost"] for v in lin.signed.values()) <= 3


def test_patches_only_touch_the_spec_file_and_describe_the_change():
    a, b = Config("resid_post", 4, 0.2), Config("mlp_out", 5, 0.3)
    p = patch_between(a, b)
    assert p.startswith("--- a/steer/spec.txt\n+++ b/steer/spec.txt\n")
    assert "-site resid_post" in p and "+site mlp_out" in p and "+layer 5" in p and "+coef_milli 300" in p
    from playbox_orchestrator.steer_search import SPEC_PATH
    assert SPEC_PATH == "steer/spec.txt" and "harness" not in p and "holdout" not in p
    assert "--- a/steer/spec.txt" in patch_between(None, a) and patch_between(a, a) == ""


def test_neighbors_stay_in_the_space_and_skip_what_was_tried():
    space = Space(layers=(2, 10), coef=(0.05, 1.0))
    c = Config("resid_post", 2, 0.05)
    rng = random.Random(0)
    got = neighbors(c, rng, space, 20, {Config("resid_post", 3, 0.05).key()})
    assert got and all(2 <= g.layer <= 10 and 0.05 <= g.coef <= 1.0 and g.key() != c.key() for g in got)
    assert Config("resid_post", 3, 0.05).key() not in {g.key() for g in got}
    assert len({g.key() for g in got}) == len(got)
    assert neighbors(c, rng, space, 20, {g.key() for g in got} | {Config("resid_post", 3, 0.05).key()}) == []


def test_the_search_is_deterministic():
    a = run(evaluator(0.05, 24, "dev"), evaluator(0.05, 32, "hold"), rounds=6, width=3)
    b = run(evaluator(0.05, 24, "dev"), evaluator(0.05, 32, "hold"), rounds=6, width=3)
    assert [(s.config, s.decision) for s in a.steps] == [(s.config, s.decision) for s in b.steps]


@pytest.mark.skipif(not BINARY.exists(), reason="target/debug/playbox-mcp is not built")
def test_the_search_against_the_real_signing_and_lineage_check():
    from playbox_orchestrator.mcp import McpClient
    from playbox_orchestrator.steer_search import McpLineage

    work = Path(tempfile.mkdtemp(prefix="pb-e4-"))
    try:
        with McpClient(BINARY, work / "custodian") as cust, McpClient(BINARY, work / "agent") as agent:
            holdout_items = [f"opener {i}" for i in range(8)]
            h = cust.call("playbox_holdout_commit", {"items": holdout_items})
            ev = cust.call("playbox_evaluator_issue", {"name": "synthetic", "version": 1, "harness": "ab" * 32, "holdout": h["root"], "command": "score", "protected": ["harness/", "holdout/"], "min_runs": 8})
            lin = McpLineage(agent, ev["evaluator"], ev["custodian"])
            r = search(root=Config("resid_post", 3, 0.0), dev=evaluator(0.05, 24, "dev"), holdout=evaluator(0.05, 32, "hold"),
                       lineage=lin, seed=1, rounds=8, width=4)
            assert landscape(r.best) > 0.3 and len(r.chain) >= 2
            # The Rust verifier replays the whole chain and agrees with every step the loop accepted.
            assert r.final_check["variants"] == len(r.chain)
            assert r.final_check["improvement"]["significant"] is True
            assert r.final_check["total_cost"] > 0
            # Editing the evaluator's files is refused by the real check.
            evil = "--- a/harness/score.py\n+++ b/harness/score.py\n@@ -1 +1 @@\n-x\n+y\n"
            v, _ = lin.sign(r.chain[0][0], evil, [0.9] * 32, 99)
            with pytest.raises(Exception, match="protected path"):
                lin.check([r.chain[0], (v, evil)])
    finally:
        shutil.rmtree(work, ignore_errors=True)


@pytest.mark.skipif(not BINARY.exists(), reason="target/debug/playbox-mcp is not built")
def test_paired_mode_accepts_a_small_consistent_gain_the_default_bound_rejects():
    """Both candidates beat the parent by the same small amount on every holdout item; item difficulty varies a lot."""
    from playbox_orchestrator.mcp import McpClient
    from playbox_orchestrator.steer_search import McpLineage

    difficulty = [0.1, 0.9, 0.35, 0.75, 0.2, 0.6, 0.05, 0.95] * 4

    def hold(c: Config) -> Result:
        gain = 0.0 if c.coef == 0 else 0.03 + 0.01 * (c.coef > 0.15)
        return Result([d + gain for d in difficulty], kl=0.1)

    outcomes = {}
    for paired in (False, True):
        work = Path(tempfile.mkdtemp(prefix="pb-e4p-"))
        try:
            with McpClient(BINARY, work / "c") as cust, McpClient(BINARY, work / "a") as agent:
                h = cust.call("playbox_holdout_commit", {"items": [f"o{i}" for i in range(32)]})
                ev = cust.call("playbox_evaluator_issue", {"name": "p", "version": 1, "harness": "ab" * 32, "holdout": h["root"], "command": "x", "protected": ["harness/"], "min_runs": 16})
                dev = lambda c: Result([0.0 if c.coef == 0 else 1.0 + c.coef] * 8, kl=0.1)  # noqa: E731
                r = search(root=Config("resid_post", 6, 0.0), dev=dev, holdout=hold, lineage=McpLineage(agent, ev["evaluator"], ev["custodian"], paired=paired),
                           space=Space(layers=(6, 6)), seed=1, rounds=3, width=3)
                outcomes[paired] = len(r.chain) - 1
        finally:
            shutil.rmtree(work, ignore_errors=True)
    assert outcomes[False] == 0, "the default bound cannot see a 0.03 gain under this much difficulty spread"
    assert outcomes[True] >= 1, "the paired bound can"


class EvidenceLineage(FakeLineage):
    """FakeLineage plus an evidence step that can be told to refuse."""
    def __init__(self, refuse=None):
        super().__init__()
        self.refuse, self.attested, self.checked = refuse, [], []

    def attest(self, variant, receipts):
        self.attested.append((variant, list(receipts)))
        return f"evidence-for-{variant}"

    def check_evidence(self, variant, evidence, receipts):
        self.checked.append((variant, evidence, list(receipts)))
        if self.refuse and self.refuse(variant, receipts):
            raise RuntimeError("the host is not pinned")
        return {"ok": True, "runs": len(receipts)}


def backed(base):
    """A holdout that returns a run for every non-zero strength, as a real steering host would."""
    def hold(c: Config) -> Result:
        r = base(c)
        if c.coef:
            r.receipt_texts = [f"receipt-{c.key()}"]
            r.outputs = [f"output-{c.key()}"]
        return r
    return hold


def test_with_evidence_required_every_accepted_variant_after_the_root_is_backed_by_a_checked_run():
    lin = EvidenceLineage()
    r = search(root=Config("resid_post", 3, 0.0), dev=evaluator(0.05, 24, "dev"), holdout=backed(evaluator(0.05, 32, "hold")),
               lineage=lin, seed=1, rounds=8, width=4, require_evidence=True)
    assert len(r.chain) >= 2 and len(r.evidence) == len(r.chain)
    assert r.evidence[0] is None, "the root is a definitional zero: there is no run to back"
    for cfg, ev in zip(r.configs[1:], r.evidence[1:]):
        assert ev["receipts"] == [f"receipt-{cfg.key()}"] and ev["outputs"] == [f"output-{cfg.key()}"]
        assert ev["evidence"].startswith("evidence-for-") and ev["verified"]["runs"] == 1
    # Evidence was requested and checked for every candidate that cleared the gain test, accepted or not.
    assert len(lin.attested) == len(lin.checked) >= len(r.chain) - 1


def test_a_gain_whose_evidence_does_not_verify_is_not_accepted():
    lin = EvidenceLineage(refuse=lambda variant, receipts: True)
    r = search(root=Config("resid_post", 3, 0.0), dev=evaluator(0.05, 24, "dev"), holdout=backed(evaluator(0.05, 32, "hold")),
               lineage=lin, seed=1, rounds=6, width=4, require_evidence=True)
    assert r.best.key() == r.root.key() and len(r.chain) == 1
    assert any("score evidence did not verify" in s.decision and "not pinned" in s.decision for s in r.steps)
    assert not any(s.decision == "accepted" for s in r.steps)


def test_a_holdout_without_a_run_cannot_back_a_gain_when_evidence_is_required():
    lin = EvidenceLineage()
    r = search(root=Config("resid_post", 3, 0.0), dev=evaluator(0.05, 24, "dev"), holdout=evaluator(0.05, 32, "hold"),
               lineage=lin, seed=1, rounds=6, width=4, require_evidence=True)
    assert len(r.chain) == 1 and any("no run to back" in s.decision for s in r.steps) and not lin.attested


def test_evidence_is_off_unless_asked_for():
    lin = EvidenceLineage()
    r = search(root=Config("resid_post", 3, 0.0), dev=evaluator(0.05, 24, "dev"), holdout=evaluator(0.05, 32, "hold"),
               lineage=lin, seed=1, rounds=6, width=4)
    assert len(r.chain) >= 2 and not lin.attested and r.evidence == [None] * len(r.chain)
