"""E1's graph and statistics against a fake backend that behaves like a simple 'model'."""
import math

import pytest

from playbox_orchestrator.steer_chain import Arm, Config, bootstrap_ci, merge_reports, percentile_against, run_e1


class FakeBackend:
    """Texts are 'positive' to the degree a steering strength was applied; reading passes a fraction on.

    Records every call so the tests can check what the graph asked for.
    """
    def __init__(self, leak=0.5):
        self.calls = []
        self.leak = leak

    def steer(self, *, direction_file, prompts, layers, coef, max_new_tokens, probe=False):
        self.calls.append({"dir": direction_file, "prompts": list(prompts), "layers": layers, "coef": coef, "probe": probe})
        digest = f"{len(self.calls):064x}"
        receipt = {"spec_digest": digest}
        if probe:
            # Score = number of 'P' characters per word, standing in for projection onto the trait.
            rows = [{"prompt": p, "projection": {str(layers[0]): p.count("P") / max(1, len(p.split()))}} for p in prompts]
            return {"output": {"probe": rows}, "receipt": receipt}
        out = []
        for p in prompts:
            level = p.count("P") * self.leak
            if direction_file == "trait" and coef > 0:
                level += coef * 4
            if "instruct" in p:
                level += 1
            out.append({"steered": " ".join(["P"] * int(round(level)) + ["w"] * 4), "clean": "w w w w", "kl_to_clean": 0.1 * coef})
        return {"output": {"results": out}, "receipt": receipt}


SEEDS = ["a b", "c d", "e f", "g h"]
ARMS = [Arm("clean", "trait", 0.0), Arm("steered", "trait", 1.0), Arm("null", "random", 1.0), Arm("prompted", "trait", 0.0, prefix="instruct ")]


def cfg(backend, hops=3):
    return Config(backend=backend, trait_direction="trait", layers=[6], probe_layer=6, hops=hops, max_new_tokens=8)


def test_only_hop_zero_is_steered_and_every_probe_uses_the_trait_direction():
    b = FakeBackend()
    run_e1(cfg(b), [Arm("clean", "trait", 0.0), Arm("steered", "trait", 1.5)], SEEDS)
    steer_calls = [c for c in b.calls if not c["probe"]]
    probe_calls = [c for c in b.calls if c["probe"]]
    assert len(steer_calls) == 2 * 3 and len(probe_calls) == 2 * 3
    nonzero = [c for c in steer_calls if c["coef"] != 0]
    assert len(nonzero) == 1 and nonzero[0]["coef"] == 1.5
    assert all(c["dir"] == "trait" and c["coef"] == 0.0 and c["layers"] == [6] for c in probe_calls)


def test_each_hop_reads_the_previous_hops_text_and_the_prefix_is_not_stored():
    b = FakeBackend()
    rep = run_e1(cfg(b, hops=2), ARMS, SEEDS)
    prompted_calls = [c for c in b.calls if not c["probe"] and c["prompts"][0].startswith("instruct ")]
    assert len(prompted_calls) == 1, "the prefix goes on hop 0 prompts only"
    for arm, hops in rep["texts"].items():
        assert not any("instruct" in t for h in hops for t in h), arm
    # Hop 1 prompts are exactly hop 0 texts of the same arm.
    steered_hop0 = rep["texts"]["steered"][0]
    assert any(c["prompts"] == steered_hop0 for c in b.calls if not c["probe"])


def test_effects_are_paired_against_clean_and_decay_with_distance():
    rep = run_e1(cfg(FakeBackend(leak=0.5)), ARMS, SEEDS)
    steered = rep["arms"]["steered"]
    assert steered[0]["effect_vs_clean"]["mean"] > 0
    means = [h["effect_vs_clean"]["mean"] for h in steered]
    assert means[0] > means[1] > means[2] > 0, means
    assert steered[0]["retained_fraction"] == pytest.approx(1.0)
    assert 0 < steered[2]["retained_fraction"] < steered[1]["retained_fraction"] < 1
    # The null direction does nothing in this world, and so does clean against itself.
    assert all(abs(h["effect_vs_clean"]["mean"]) < 1e-9 for h in rep["arms"]["null"])
    assert all(h["effect_vs_clean"]["mean"] == 0 for h in rep["arms"]["clean"])
    assert rep["arms"]["clean"][0]["retained_fraction"] is None
    assert rep["arms"]["steered"][0]["kl_mean"] > 0 == rep["arms"]["clean"][0]["kl_mean"]


def test_no_leak_means_no_effect_at_later_hops():
    rep = run_e1(cfg(FakeBackend(leak=0.0)), ARMS, SEEDS)
    steered = rep["arms"]["steered"]
    assert steered[0]["effect_vs_clean"]["mean"] > 0
    assert steered[1]["effect_vs_clean"]["mean"] == 0 and steered[2]["effect_vs_clean"]["mean"] == 0


def test_a_clean_arm_is_required_and_result_count_is_checked():
    with pytest.raises(ValueError):
        run_e1(cfg(FakeBackend()), [Arm("steered", "trait", 1.0)], SEEDS)

    class Short(FakeBackend):
        def steer(self, **kw):
            r = super().steer(**kw)
            if not kw.get("probe", False):
                r["output"]["results"] = r["output"]["results"][:-1]
            return r

    with pytest.raises(RuntimeError, match="results for"):
        run_e1(cfg(Short()), ARMS, SEEDS)


def test_empty_model_output_becomes_a_placeholder_and_is_counted():
    class Silent(FakeBackend):
        def steer(self, **kw):
            r = super().steer(**kw)
            if not kw.get("probe", False):
                for row in r["output"]["results"]:
                    row["steered"] = "   "
            return r

    rep = run_e1(cfg(Silent(), hops=2), [Arm("clean", "trait", 0.0)], SEEDS)
    assert rep["arms"]["clean"][0]["empty_texts"] == len(SEEDS)


def test_bootstrap_ci_and_percentile():
    m, lo, hi = bootstrap_ci([1.0] * 20)
    assert (m, lo, hi) == (1.0, 1.0, 1.0)
    xs = [0.0, 1.0] * 50
    m, lo, hi = bootstrap_ci(xs, seed=1)
    assert m == 0.5 and 0.35 < lo < 0.5 < hi < 0.65
    assert bootstrap_ci(xs, seed=1) == bootstrap_ci(xs, seed=1)
    assert all(math.isnan(v) for v in bootstrap_ci([]))
    assert percentile_against(5, [1, 2, 3, 4]) == 1.0 and percentile_against(0, [1, 2]) == 0.0
    assert math.isnan(percentile_against(1, []))


def test_null_population_ranks_steered_and_prompted():
    arms = ARMS[:2] + [Arm("prompted", "trait", 0.0, prefix="instruct ")] + [Arm(f"null{k}", "random", 1.0) for k in range(4)]
    rep = run_e1(cfg(FakeBackend()), arms, SEEDS)
    row = rep["null_population"][0]
    assert row["n"] == 4 and row["steered_percentile"] == 1.0 and row["null_sd"] >= 0
    assert row["steered_z"] is None or row["steered_z"] > 0
    assert "prompted_percentile" in row
    assert run_e1(cfg(FakeBackend()), ARMS[:2], SEEDS)["null_population"] == [], "no nulls, no population"


class Noisy(FakeBackend):
    """Random directions have a small, direction-dependent effect, like the real thing."""
    def steer(self, *, direction_file, prompts, layers, coef, max_new_tokens, probe=False):
        r = super().steer(direction_file=direction_file, prompts=prompts, layers=layers, coef=coef, max_new_tokens=max_new_tokens, probe=probe)
        if not probe and direction_file.startswith("random") and coef > 0:
            for row in r["output"]["results"]:
                row["steered"] = " ".join(["P"] * int(direction_file[-1]) + ["w"] * 4)
        return r


def test_shards_merge_and_disagreement_is_an_error():
    def shard(ks):
        return run_e1(cfg(Noisy()), [Arm("clean", "trait", 0.0), Arm("steered", "trait", 1.0)] + [Arm(f"null{k}", f"random{k}", 1.0) for k in ks], SEEDS)

    a, b, whole = shard([0, 1]), shard([2, 3]), shard([0, 1, 2, 3])
    merged = merge_reports([a, b])
    assert sorted(n for n in merged["arms"] if n.startswith("null")) == ["null0", "null1", "null2", "null3"]
    assert merged["null_population"] == whole["null_population"]
    assert merged["arms"]["steered"][0]["projection"] == whole["arms"]["steered"][0]["projection"]
    with pytest.raises(ValueError, match="two shards ran"):
        merge_reports([a, shard([1])])
    other = shard([2])
    other["seeds"] = 99
    with pytest.raises(ValueError, match="disagree on seeds"):
        merge_reports([a, other])
    tampered = shard([2])
    tampered["arms"]["clean"][0]["projection"][0] += 1
    with pytest.raises(ValueError, match="not deterministic"):
        merge_reports([a, tampered])
