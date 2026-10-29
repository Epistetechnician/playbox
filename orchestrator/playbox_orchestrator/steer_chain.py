"""E1 from docs/steering.md: does a trait steered into one agent spread to agents that only read its text?

    seeds ──► hop 0 (steered agent A) ──► hop 1 (B reads A's text) ──► hop 2 (C reads B's text) ──► ...

Every hop is one steering run on the same hosted model; only hop 0 has a nonzero strength. After each hop the
generated texts are *probed*: fed back through the model and projected onto the trait direction. Arms:

- `steered`  hop 0 uses the trait direction at strength `coef`.
- `null`     hop 0 uses a random unit direction of the same strength (the matched-norm control).
- `clean`    no steering anywhere.
- `prompted` no steering; hop 0's prompts are prefixed with an instruction toward the trait (the baseline
             AxBench found hard to beat). The prefix is not part of the stored text.

Everything the model says is untrusted text; nothing here acts on it. Each run carries the host's signed
receipt digest, so a report can be checked against what the host says it ran.
"""
from __future__ import annotations

import math
import operator
import random
import statistics
from dataclasses import dataclass, field
from typing import Annotated, Protocol, TypedDict

from langgraph.graph import END, START, StateGraph


class SteerBackend(Protocol):
    def steer(self, *, direction_file: str, prompts: list[str], layers: list[int], coef: float, max_new_tokens: int, probe: bool = False) -> dict: ...


@dataclass
class McpSteerBackend:
    """`playbox_steer` through a sidecar that has joined a steering room."""
    mcp: object
    weights: str
    model: str = "model"
    site: str = "resid_post"
    seed: int = 0

    def steer(self, *, direction_file, prompts, layers, coef, max_new_tokens, probe=False):
        return self.mcp.call("playbox_steer", {
            "weights": self.weights, "model": self.model, "direction_file": direction_file, "prompts": prompts,
            "layers": layers, "coef": coef, "max_new_tokens": max_new_tokens, "probe": probe, "site": self.site, "seed": self.seed,
        })


@dataclass(frozen=True)
class Arm:
    name: str
    direction_file: str       # used at hop 0 (the intervention); later hops use strength 0
    coef: float               # hop 0 strength in activation norms
    prefix: str = ""          # prepended to hop 0 prompts only


@dataclass
class Hop:
    index: int
    texts: list[str]
    projection: list[float]   # trait projection of each text, in norms of the probe layer
    kl: list[float]           # mean per-token KL(steered || clean) of the run that produced each text
    receipts: list[str] = field(default_factory=list)   # spec digests of the runs behind this hop


class State(TypedDict, total=False):
    arm: Arm
    seeds: list[str]
    hops: Annotated[list[Hop], operator.add]


@dataclass
class Config:
    backend: SteerBackend
    trait_direction: str      # always the real trait direction: it is what we measure with
    layers: list[int]         # where hop 0 is steered
    probe_layer: int
    hops: int = 3
    max_new_tokens: int = 24
    reader_suffix: str = ""   # appended to the previous text to make the next hop's prompt


def _clean_text(t: str) -> str:
    t = " ".join(t.split())
    return t if t else "."


def _hop_node(cfg: Config, i: int):
    def node(state: State) -> dict:
        arm, hops = state["arm"], state.get("hops", [])
        if i == 0:
            prompts = [arm.prefix + s for s in state["seeds"]]
            direction, coef = arm.direction_file, arm.coef
        else:
            prompts = [t + cfg.reader_suffix for t in hops[-1].texts]
            direction, coef = arm.direction_file, 0.0
        gen = cfg.backend.steer(direction_file=direction, prompts=prompts, layers=cfg.layers, coef=coef, max_new_tokens=cfg.max_new_tokens)
        rows = gen["output"]["results"]
        if len(rows) != len(prompts):
            raise RuntimeError(f"host returned {len(rows)} results for {len(prompts)} prompts")
        texts = [_clean_text(r["steered"]) for r in rows]
        probe = cfg.backend.steer(direction_file=cfg.trait_direction, prompts=texts, layers=[cfg.probe_layer], coef=0.0, max_new_tokens=1, probe=True)
        proj = [row["projection"][str(cfg.probe_layer)] for row in probe["output"]["probe"]]
        return {"hops": [Hop(i, texts, proj, [r.get("kl_to_clean", 0.0) for r in rows], [gen["receipt"]["spec_digest"], probe["receipt"]["spec_digest"]])]}
    return node


def build_chain(cfg: Config):
    g = StateGraph(State)
    names = [f"hop{i}" for i in range(cfg.hops)]
    for i, n in enumerate(names):
        g.add_node(n, _hop_node(cfg, i))
    g.add_edge(START, names[0])
    for a, b in zip(names, names[1:]):
        g.add_edge(a, b)
    g.add_edge(names[-1], END)
    return g.compile()


# ---- statistics --------------------------------------------------------------------------------------


def bootstrap_ci(xs: list[float], n: int = 2000, seed: int = 0, level: float = 0.95) -> tuple[float, float, float]:
    """(mean, low, high) of the mean, by percentile bootstrap over items."""
    if not xs:
        return (math.nan, math.nan, math.nan)
    rng = random.Random(seed)
    means = sorted(statistics.fmean(rng.choices(xs, k=len(xs))) for _ in range(n))
    lo, hi = means[int((1 - level) / 2 * n)], means[min(n - 1, int((1 + level) / 2 * n))]
    return (statistics.fmean(xs), lo, hi)


def percentile_against(value: float, population: list[float]) -> float:
    """Fraction of `population` strictly below `value` (a rank against random-direction controls)."""
    return sum(p < value for p in population) / len(population) if population else math.nan


def run_arm(cfg: Config, arm: Arm, seeds: list[str]) -> list[Hop]:
    return build_chain(cfg).invoke({"arm": arm, "seeds": seeds, "hops": []})["hops"]


def run_e1(cfg: Config, arms: list[Arm], seeds: list[str], boot_seed: int = 0) -> dict:
    """Run every arm over the same seeds and summarize per hop, with paired differences against `clean`."""
    if "clean" not in {a.name for a in arms}:
        raise ValueError("an arm named `clean` is required: every effect is measured against it")
    raw = {a.name: run_arm(cfg, a, seeds) for a in arms}
    clean = raw["clean"]
    report: dict = {"seeds": len(seeds), "hops": cfg.hops, "layers": cfg.layers, "probe_layer": cfg.probe_layer, "arms": {}}
    for name, hops in raw.items():
        per_hop = []
        for h, c in zip(hops, clean):
            diff = [a - b for a, b in zip(h.projection, c.projection)]   # paired by seed
            mean, lo, hi = bootstrap_ci(diff, seed=boot_seed)
            kmean, _, _ = bootstrap_ci(h.kl, seed=boot_seed)
            per_hop.append({
                "hop": h.index, "projection_mean": statistics.fmean(h.projection),
                "effect_vs_clean": {"mean": mean, "low": lo, "high": hi},
                "kl_mean": kmean,
                "empty_texts": sum(t == "." for t in h.texts),
                "spec_digests": h.receipts,
            })
        report["arms"][name] = per_hop
    # How much of hop 0's effect is still there at each later hop.
    for name in report["arms"]:
        h0 = report["arms"][name][0]["effect_vs_clean"]["mean"]
        for row in report["arms"][name]:
            row["retained_fraction"] = (row["effect_vs_clean"]["mean"] / h0) if abs(h0) > 1e-9 else None
    report["texts"] = {n: [h.texts for h in hops] for n, hops in raw.items()}
    return report
