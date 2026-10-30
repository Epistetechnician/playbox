"""E4 from docs/steering.md: a search over steering configurations whose gains are checked, not just claimed.

Roles, each a separate key:
- the **searcher** proposes configurations, tries them on a *dev* prompt set it can see, and signs variants;
- the **custodian** sealed a holdout before the search began, issued the evaluator, and is the only one who runs
  candidates on the holdout. The searcher sees per-prompt scores and nothing else about it.

A step of the loop:  propose neighbors of the current best  ->  score them on dev  ->  keep the best that stays
under the KL cap  ->  if it beats the current dev score, ask the custodian to score it on the holdout  ->  sign a
variant whose patch is the change to the spec file and whose scores are the holdout scores  ->  accept it only if
the lineage says its gain over its parent has a positive one-sided lower bound.

Every holdout query is counted as cost in the variant, because a fixed holdout is eventually overfit by repeated
queries; `max_holdout_queries` bounds them. The patch only ever touches `steer/spec.txt`, so the evaluator's
protected paths (`harness/`, `holdout/`) are never edited by construction, and the lineage refuses a variant that
would.

What this proves is the procedure. The scores in a variant are the author's claim; they are only as good as the
custodian's run that produced them.
"""
from __future__ import annotations

import difflib
import random
import statistics
from dataclasses import dataclass, field
from typing import Callable, Protocol

SPEC_PATH = "steer/spec.txt"
SITES = ("resid_post", "mlp_out")


@dataclass(frozen=True)
class Config:
    site: str
    layer: int
    coef: float          # in activation norms

    def spec_file(self) -> str:
        return f"site {self.site}\nlayer {self.layer}\ncoef_milli {round(self.coef * 1000)}\n"

    def key(self) -> tuple:
        return (self.site, self.layer, round(self.coef * 1000))


@dataclass
class Result:
    scores: list[float]   # one per prompt
    kl: float             # mean per-token KL(steered || clean), nats
    receipts: list[str] = field(default_factory=list)          # spec digests of the runs, for the log
    receipt_texts: list[str] = field(default_factory=list)     # the exact signed receipt records, for evidence
    outputs: list[str] = field(default_factory=list)           # the exact engine outputs those receipts hash

    @property
    def mean(self) -> float:
        return statistics.fmean(self.scores)


def patch_between(parent: Config | None, child: Config) -> str:
    """The unified diff turning the parent's spec file into the child's (empty parent: a new file)."""
    old = parent.spec_file().splitlines(keepends=True) if parent else []
    new = child.spec_file().splitlines(keepends=True)
    return "".join(difflib.unified_diff(old, new, f"a/{SPEC_PATH}", f"b/{SPEC_PATH}"))


@dataclass(frozen=True)
class Space:
    layers: tuple[int, int] = (2, 10)
    coef: tuple[float, float] = (0.05, 1.0)
    sites: tuple[str, ...] = SITES


def neighbors(c: Config, rng: random.Random, space: Space, n: int, exclude: set[tuple]) -> list[Config]:
    """Up to `n` distinct single-step mutations of `c` inside `space`, none already tried."""
    moves: list[Config] = []
    for d in (-2, -1, 1, 2):
        moves.append(Config(c.site, c.layer + d, c.coef))
    if c.coef == 0:
        # From "no steering" the only useful move is to turn it on, at a few strengths.
        moves += [Config(c.site, c.layer, x) for x in (0.1, 0.2, 0.4)]
    else:
        for f in (1 / 1.5, 1.5, 1 / 2.0, 2.0):
            moves.append(Config(c.site, c.layer, round(c.coef * f, 3)))
    for site in space.sites:
        if site != c.site:
            moves.append(Config(site, c.layer, c.coef))
    ok = [m for m in moves if space.layers[0] <= m.layer <= space.layers[1] and (m.coef == 0 or space.coef[0] <= m.coef <= space.coef[1]) and m.site in space.sites and m.key() not in exclude]
    rng.shuffle(ok)
    return ok[:n]


class LineageBackend(Protocol):
    def sign(self, parent: str | None, patch: str, scores: list[float], cost: int) -> tuple[str, str]: ...
    def check(self, chain: list[tuple[str, str]]) -> dict: ...
    # Needed only when a search runs with require_evidence: the custodian signs which runs back a variant's scores,
    # and the verifier checks that against the hosts, weights and image it pins (raises if it does not hold).
    def attest(self, variant: str, receipts: list[str]) -> str: ...
    def check_evidence(self, variant: str, evidence: str, receipts: list[str]) -> dict: ...


@dataclass
class Step:
    round: int
    config: Config
    dev_mean: float
    dev_kl: float
    decision: str
    holdout_mean: float | None = None
    lower_bound: float | None = None
    variant: str | None = None


@dataclass
class SearchResult:
    root: Config
    best: Config
    steps: list[Step]
    chain: list[tuple[str, str]]          # (variant text, patch), oldest first
    configs: list[Config]                 # the config of each variant in the chain
    holdout_queries: int
    dev_queries: int
    final_check: dict
    # One entry per variant in `chain`: the custodian's evidence and the material to check it. The root is a
    # definitional zero (no steering, so the steered text is the clean text) and has no run to back.
    evidence: list[dict | None] = field(default_factory=list)


def search(
    *,
    root: Config,
    dev: Callable[[Config], Result],
    holdout: Callable[[Config], Result],
    lineage: LineageBackend,
    space: Space = Space(),
    rounds: int = 6,
    width: int = 3,
    kl_cap: float = 0.6,
    margin: float = 0.0,
    max_holdout_queries: int = 12,
    seed: int = 0,
    z: float = 1.645,
    require_evidence: bool = False,
    log: Callable[[str], None] = lambda _: None,
) -> SearchResult:
    rng = random.Random(seed)
    tried: set[tuple] = {root.key()}
    root_hold = holdout(root)
    queries = 1
    root_text, _ = lineage.sign(None, patch_between(None, root), root_hold.scores, 1)
    chain = [(root_text, patch_between(None, root))]
    configs = [root]
    evidence: list[dict | None] = [None]
    cur, cur_text, cur_dev = root, root_text, dev(root).mean
    dev_queries = 1
    steps: list[Step] = []
    for r in range(rounds):
        cands = neighbors(cur, rng, space, width, tried)
        if not cands:
            log(f"round {r}: no untried neighbors")
            break
        scored = []
        for c in cands:
            tried.add(c.key())
            res = dev(c)
            dev_queries += 1
            scored.append((c, res))
            log(f"round {r}: dev {c} -> {res.mean:+.4f} kl={res.kl:.3f}")
        feasible = [(c, x) for c, x in scored if x.kl <= kl_cap]
        for c, x in scored:
            if x.kl > kl_cap:
                steps.append(Step(r, c, x.mean, x.kl, "rejected: KL over the cap"))
        if not feasible:
            continue
        best, bres = max(feasible, key=lambda t: t[1].mean)
        if bres.mean <= cur_dev + margin:
            steps.append(Step(r, best, bres.mean, bres.kl, "not better on dev; not sent to the holdout"))
            continue
        if queries >= max_holdout_queries:
            steps.append(Step(r, best, bres.mean, bres.kl, "holdout query budget spent"))
            break
        hres = holdout(best)
        queries += 1
        if hres.kl > kl_cap:
            steps.append(Step(r, best, bres.mean, bres.kl, "rejected: KL over the cap on the holdout", hres.mean))
            continue
        patch = patch_between(cur, best)
        text, _ = lineage.sign(cur_text, patch, hres.scores, queries)   # cost: holdout queries spent so far
        verdict = lineage.check(chain + [(text, patch)])
        imp = verdict.get("improvement") or {}
        lb = imp.get("lower_bound")
        backed = None
        if imp.get("significant") and require_evidence:
            # The custodian vouches for the runs behind these scores, and the verifier checks the vouching against
            # the hosts, weights and engine image it pins. A gain nobody can trace to a run is not accepted.
            if not hres.receipt_texts:
                steps.append(Step(r, best, bres.mean, bres.kl, "rejected: the holdout result carries no run to back its scores", hres.mean, lb))
                log(f"round {r}: rejected {best}: no receipts")
                continue
            ev = lineage.attest(text, hres.receipt_texts)
            try:
                proof = lineage.check_evidence(text, ev, hres.receipt_texts)
            except Exception as e:  # the verifier's refusal is the answer, whatever its type
                steps.append(Step(r, best, bres.mean, bres.kl, f"rejected: score evidence did not verify ({e})", hres.mean, lb))
                log(f"round {r}: rejected {best}: evidence did not verify: {e}")
                continue
            backed = {"evidence": ev, "receipts": hres.receipt_texts, "outputs": hres.outputs, "verified": proof}
        if imp.get("significant"):
            chain.append((text, patch))
            evidence.append(backed)
            configs.append(best)
            cur, cur_text, cur_dev = best, text, bres.mean
            steps.append(Step(r, best, bres.mean, bres.kl, "accepted", hres.mean, lb, text))
            log(f"round {r}: ACCEPTED {best} holdout {hres.mean:+.4f} lower bound {lb:+.4f}")
        else:
            steps.append(Step(r, best, bres.mean, bres.kl, "rejected: gain over the parent not supported on the holdout", hres.mean, lb))
            log(f"round {r}: rejected {best} holdout {hres.mean:+.4f} lower bound {lb}")
    return SearchResult(root, cur, steps, chain, configs, queries, dev_queries, lineage.check(chain), evidence)


# ---- the real lineage backend: two sidecars, so signing and issuing are different keys ----------------------


class McpLineage:
    """Signs variants with the agent sidecar's key and replays chains in Rust against the pinned custodian."""

    def __init__(self, agent, evaluator: str, custodian_key: str, paired: bool = False, custodian=None, hosts=None,
                 weights: str | None = None, image: str | None = None, min_ceiling: str = "local_internal_exploratory"):
        # `paired` is this verifier's assertion that the evaluator scores the same holdout items, in the same order,
        # for every variant. It is what makes the tighter bound valid; leave it off if scores are repeated runs.
        self.agent, self.evaluator, self.custodian_key, self.paired = agent, evaluator, custodian_key, paired
        # Evidence: `custodian` is the sidecar holding the evaluator's key (it signs); `agent` verifies, pinning the
        # `hosts` whose receipts count and, if given, the weights and engine image every run must have used.
        self.custodian, self.hosts, self.weights, self.image, self.min_ceiling = custodian, hosts or [], weights, image, min_ceiling

    def attest(self, variant: str, receipts: list[str]) -> str:
        if self.custodian is None:
            raise RuntimeError("no custodian sidecar was given, so nothing can sign evidence")
        return self.custodian.call("playbox_evidence_sign", {"evaluator": self.evaluator, "variant": variant, "receipts": receipts})["evidence"]

    def check_evidence(self, variant: str, evidence: str, receipts: list[str]) -> dict:
        args = {"evidence": evidence, "evaluator": self.evaluator, "variant": variant, "custodians": [self.custodian_key],
                "hosts": self.hosts, "receipts": receipts, "min_ceiling": self.min_ceiling}
        if self.weights:
            args["weights"] = self.weights
        if self.image:
            args["image"] = self.image
        return self.agent.call("playbox_evidence_check", args)

    def sign(self, parent, patch, scores, cost):
        args = {"evaluator": self.evaluator, "custodians": [self.custodian_key], "patch": patch, "scores": scores, "cost": cost}
        if parent:
            args["parent"] = parent
        out = self.agent.call("playbox_variant_sign", args)
        return out["variant"], out["id"]

    def check(self, chain):
        return self.agent.call("playbox_lineage_check", {
            "evaluators": [self.evaluator], "custodians": [self.custodian_key],
            "variants": [{"variant": v, "patch": p} for v, p in chain], "paired": self.paired,
        })
