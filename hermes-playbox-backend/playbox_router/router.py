"""Metered intelligence-per-cost routing, 3-way patch composition, and sealed-gate selection."""
from __future__ import annotations

import ast
import hashlib
import re
import shutil
import subprocess
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Sequence

_BROKEN_REPRO_EXC_RE = re.compile(
    r"(?m)^\s*(SyntaxError|IndentationError|TabError|ImportError|ModuleNotFoundError|NameError)\s*:"
)


def is_genuine_repro_failure(base_output: str, script: str = "") -> tuple[bool, str]:
    """Verify that a reproduction script's failure on `base_commit` is a genuine behavioral/assertion
    failure rather than a broken script (`SyntaxError`, `ImportError`, `ModuleNotFoundError`, `NameError`)."""
    if script.strip():
        try:
            ast.parse(script)
        except SyntaxError as exc:
            return False, f"SyntaxError in repro script: {exc.msg} (line {exc.lineno})"
    m = _BROKEN_REPRO_EXC_RE.search(base_output or "")
    if m:
        return False, f"repro failed with {m.group(1)} on base_commit"
    return True, "ok"


@dataclass
class BudgetCap:
    """Hard USD spending cap across a multi-worker + compose + escalation run."""
    max_total_usd: float = 0.50
    max_worker_usd: float = 0.15
    escalate_reserve_usd: float = 0.10
    spent_usd: float = 0.0

    def remaining(self) -> float:
        return max(0.0, self.max_total_usd - self.spent_usd)

    def can_launch_worker(self) -> bool:
        return self.remaining() >= min(self.max_worker_usd, 0.01)

    def can_escalate(self) -> bool:
        return self.remaining() >= self.escalate_reserve_usd and self.escalate_reserve_usd > 0.0

    def charge(self, usd: float) -> None:
        if usd > 0:
            self.spent_usd += float(usd)


@dataclass
class CandidateResult:
    """One worker's produced patch + metered cost + sealed-gate verification outcome."""
    worker_id: str
    model: str
    patch: str
    cost_usd: float
    wall_s: float = 0.0
    score: float = 0.0
    passed: bool = False
    audit_verified: bool = False
    policy_digest: str = ""
    meta: dict = field(default_factory=dict)

    @property
    def patch_sha256(self) -> str:
        return hashlib.sha256(self.patch.encode("utf-8", "ignore")).hexdigest()

    @property
    def value_per_dollar(self) -> float:
        return self.score / max(self.cost_usd, 1e-6)


@dataclass
class ReproGateResult:
    """Host-sealed independent reproduction test produced without seeing any solver's patch.

    Anti-vacuous & sanity invariant: `valid` is True ONLY when `script` is non-empty, parses cleanly,
    failed against the unpatched base repository (`fails_on_base=True`), and did NOT fail due to
    `SyntaxError` / `ImportError` / `ModuleNotFoundError` / `NameError`.
    """
    script: str
    fails_on_base: bool
    cost_usd: float = 0.0
    base_output: str = ""
    worker_id: str = "w_repro"
    sanity_ok: bool = True
    reject_reason: str = ""

    @property
    def valid(self) -> bool:
        if not self.script.strip() or not self.fails_on_base or not self.sanity_ok:
            return False
        ok, _ = is_genuine_repro_failure(self.base_output, self.script)
        return ok


@dataclass
class RouteDecision:
    """Final verified decision emitted by the intelligence-per-cost router."""
    winner: CandidateResult | None
    composed_tried: bool
    composed_won: bool
    needs_escalation: bool
    total_cost_usd: float
    candidates_evaluated: int
    provenance: dict
    escalated: bool = False
    escalation_won: bool = False


def _touched_files(patch: str) -> set[str]:
    return set(re.findall(r"(?m)^diff --git a/(.+?) b/", patch or ""))


def compose_patches(base_repo: str | Path, patches: Sequence[str]) -> tuple[str | None, list[int]]:
    """Deterministically compose multiple candidate patches over `base_repo` via clean git 3-way merge.

    Returns `(combined_patch, merged_indices)` or `(None, [])` if fewer than 2 patches merged cleanly.
    Runs in an isolated throwaway copy with git hooks/filters disabled (`core.hooksPath=/dev/null`).
    """
    non_empty = [(i, p) for i, p in enumerate(patches) if p and p.strip()]
    if len(non_empty) < 2:
        return None, []

    git_safe = [
        "git",
        "-c", "core.hooksPath=/dev/null",
        "-c", "core.fsmonitor=false",
        "-c", "core.symlinks=false",
        "-c", "core.attributesFile=/dev/null",
    ]
    tmp = Path(tempfile.mkdtemp(prefix="pbx-compose-"))
    try:
        work = tmp / "repo"
        shutil.copytree(base_repo, work, symlinks=False, ignore=shutil.ignore_patterns(".git"))
        subprocess.run([*git_safe, "init", "-q", "-b", "main", str(work)], check=True, capture_output=True)
        subprocess.run([*git_safe, "-C", str(work), "config", "user.name", "playbox-compose"], check=True, capture_output=True)
        subprocess.run([*git_safe, "-C", str(work), "config", "user.email", "compose@playbox.local"], check=True, capture_output=True)
        subprocess.run([*git_safe, "-C", str(work), "add", "-A"], check=True, capture_output=True)
        subprocess.run([*git_safe, "-C", str(work), "commit", "-q", "-m", "base"], check=True, capture_output=True)

        merged: list[int] = []
        for idx, patch in non_empty:
            # Dry-run 3-way apply first so a conflicting patch never dirties the working tree.
            chk = subprocess.run(
                [*git_safe, "-C", str(work), "apply", "--check", "--3way", "--whitespace=nowarn", "-"],
                input=patch,
                text=True,
                capture_output=True,
            )
            if chk.returncode != 0:
                continue
            ap = subprocess.run(
                [*git_safe, "-C", str(work), "apply", "--3way", "--whitespace=nowarn", "-"],
                input=patch,
                text=True,
                capture_output=True,
            )
            if ap.returncode == 0:
                merged.append(idx)
        if len(merged) < 2:
            return None, merged
        subprocess.run([*git_safe, "-C", str(work), "add", "-A"], check=True, capture_output=True)
        diff = subprocess.run(
            [*git_safe, "-C", str(work), "diff", "--cached", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        return (diff if diff.strip() else None), merged
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


# Verifier signature: `(patch_text) -> (passed: bool, score: float, meta: dict)`
SealedVerifier = Callable[[str], tuple[bool, float, dict]]


def select_and_compose(
    base_repo: str | Path,
    candidates: Sequence[CandidateResult],
    verify: SealedVerifier,
    budget: BudgetCap,
) -> RouteDecision:
    """Evaluate verified candidates through a fresh-sandbox sealed gate, try 3-way composition on
    top distinct candidates if none passes yet (or to improve score), and decide whether escalation is warranted.

    Security invariant: candidates whose host `audit_verified` is False are rejected before selection.
    """
    verified: list[CandidateResult] = []
    seen_hashes: set[str] = set()
    for c in candidates:
        budget.charge(c.cost_usd)
        if not c.audit_verified or not c.patch.strip():
            continue
        h = c.patch_sha256
        if h in seen_hashes:
            continue
        seen_hashes.add(h)
        passed, score, meta = verify(c.patch)
        verified.append(CandidateResult(
            worker_id=c.worker_id,
            model=c.model,
            patch=c.patch,
            cost_usd=c.cost_usd,
            wall_s=c.wall_s,
            score=score,
            passed=passed,
            audit_verified=c.audit_verified,
            policy_digest=c.policy_digest,
            meta=meta,
        ))

    verified.sort(key=lambda r: (r.passed, r.score, -r.cost_usd), reverse=True)
    best = verified[0] if verified else None

    composed_tried = False
    composed_won = False
    composed_parents: list[str] = []

    # If no single cheap worker achieved full pass (or top candidates edit disjoint files), try Harness²-style Compose.
    if len(verified) >= 2 and (best is None or not best.passed):
        top = verified[:4]
        merged_patch, merged_idxs = compose_patches(base_repo, [c.patch for c in top])
        if merged_patch and hashlib.sha256(merged_patch.encode()).hexdigest() not in seen_hashes:
            composed_tried = True
            composed_parents = [top[i].worker_id for i in merged_idxs]
            passed, score, meta = verify(merged_patch)
            comp = CandidateResult(
                worker_id="compose(" + "+".join(composed_parents) + ")",
                model="deterministic-3way",
                patch=merged_patch,
                cost_usd=0.0,
                score=score,
                passed=passed,
                audit_verified=True,
                policy_digest=top[0].policy_digest,
                meta={"parents": composed_parents, **meta},
            )
            if best is None or (comp.passed, comp.score) > (best.passed, best.score):
                best = comp
                composed_won = True

    needs_escalation = (best is None or not best.passed) and budget.can_escalate()
    provenance = {
        "domain": "playbox.router.v1",
        "candidates": [
            {
                "worker_id": r.worker_id,
                "model": r.model,
                "patch_sha256": r.patch_sha256,
                "passed": r.passed,
                "score": r.score,
                "cost_usd": round(r.cost_usd, 6),
                "files": sorted(_touched_files(r.patch)),
                "audit_verified": r.audit_verified,
                "policy_digest": r.policy_digest,
            }
            for r in verified
        ],
        "composed_parents": composed_parents,
        "winner": best.worker_id if best else None,
        "winner_sha256": best.patch_sha256 if best else None,
        "total_cost_usd": round(budget.spent_usd, 6),
    }
    return RouteDecision(
        winner=best,
        composed_tried=composed_tried,
        composed_won=composed_won,
        needs_escalation=needs_escalation,
        total_cost_usd=budget.spent_usd,
        candidates_evaluated=len(verified),
        provenance=provenance,
    )


def _diff_hunk_headers(patch: str, limit: int = 6) -> list[str]:
    headers = [line.strip() for line in (patch or "").splitlines() if line.startswith("@@")]
    return headers[:limit]


def build_escalation_brief(
    atlas_brief: str,
    prior_candidates: Sequence[CandidateResult],
    *,
    repro_feedback: str = "",
    max_chars: int = 3200,
) -> str:
    """Build a compact, zero-oracle-leakage escalation brief for a frontier model.

    Summarizes the verified AtlasKV call-chain, what cheaper workers already tried (files, hunk
    locations, and online-gate failure excerpt — never any hidden holdout test), and asks the
    escalated model to investigate deeper root causes along the call chain.
    """
    parts: list[str] = []
    if atlas_brief:
        parts.append(atlas_brief)
    lines = ["[Playbox Escalation Context — prior cheap attempts did not clear the online gate]"]
    for c in prior_candidates:
        files = sorted(_touched_files(c.patch))
        hunks = _diff_hunk_headers(c.patch, limit=3)
        status = f"score={c.score:.2f}, audit={'ok' if c.audit_verified else 'UNVERIFIED'}"
        fail_tail = str(c.meta.get("online_failure") or "")[:240].replace("\n", " ")
        lines.append(
            f"- {c.worker_id} ({c.model}, {status}): files={files or ['none']} hunks={hunks or ['none']}"
            + (f" | gate_note: {fail_tail}" if fail_tail else "")
        )
    if repro_feedback:
        lines.append("Independent reproduction failure (non-oracle):")
        lines.append(repro_feedback.strip()[:800])
    lines.append(
        "Avoid repeating the same surface workaround; trace the root cause down the AtlasKV call chain "
        "(prefer fixing the underlying core helper/data path so both direct unit callers and end-to-end flows succeed)."
    )
    parts.append("\n".join(lines))
    combined = "\n\n".join(parts)
    return combined[:max_chars]


# Escalation callback: `(escalation_brief: str, remaining_usd: float) -> CandidateResult | None`
EscalationFn = Callable[[str, float], CandidateResult | None]


def run_cascade(
    base_repo: str | Path,
    cheap_candidates: Sequence[CandidateResult],
    *,
    online_verify: SealedVerifier,
    budget: BudgetCap,
    escalate_fn: EscalationFn | None = None,
    atlas_brief: str = "",
    repro_gate: ReproGateResult | None = None,
) -> RouteDecision:
    """Two-tier metered cascade: cheap parallel workers + 3-way compose -> conditional frontier escalation.

    Security & anti-Goodhart invariants:
    - `online_verify` must ONLY use non-oracle checks (existing repo tests + optional non-vacuous
      independent repro gate), never hidden evaluation holdouts.
    - Candidates with `audit_verified=False` are dropped at both cheap and escalated stages.
    - Escalation triggers ONLY when cheap workers + deterministic composition fail `online_verify`
      AND `budget.can_escalate()` is True.
    """
    if repro_gate is not None:
        budget.charge(repro_gate.cost_usd)

    stage1 = select_and_compose(base_repo, cheap_candidates, online_verify, budget)
    stage1.provenance["escalated"] = False
    stage1.provenance["escalation_won"] = False
    if repro_gate is not None:
        ok_sanity, reason = is_genuine_repro_failure(repro_gate.base_output, repro_gate.script)
        stage1.provenance["repro_gate"] = {
            "valid": repro_gate.valid,
            "fails_on_base": repro_gate.fails_on_base,
            "sanity_ok": repro_gate.sanity_ok and ok_sanity,
            "reject_reason": repro_gate.reject_reason or ("" if ok_sanity else reason),
            "cost_usd": round(repro_gate.cost_usd, 6),
        }

    if not stage1.needs_escalation or escalate_fn is None:
        return stage1

    repro_fb = ""
    if stage1.winner and stage1.winner.meta:
        repro_fb = str(stage1.winner.meta.get("online_failure") or "")
    elif repro_gate and repro_gate.valid and repro_gate.base_output:
        repro_fb = repro_gate.base_output

    # Collect evaluated cheap candidates with their online scores/failure notes for the brief.
    prior_for_brief = list(cheap_candidates)
    brief = build_escalation_brief(atlas_brief, prior_for_brief, repro_feedback=repro_fb)
    esc = escalate_fn(brief, budget.remaining())
    if esc is None:
        return stage1

    budget.charge(esc.cost_usd)
    if not esc.audit_verified or not esc.patch.strip():
        stage1.provenance["escalated"] = True
        stage1.provenance["escalation_rejected"] = "unverified_audit_or_empty"
        stage1.provenance["total_cost_usd"] = round(budget.spent_usd, 6)
        return RouteDecision(
            winner=stage1.winner,
            composed_tried=stage1.composed_tried,
            composed_won=stage1.composed_won,
            needs_escalation=False,
            total_cost_usd=budget.spent_usd,
            candidates_evaluated=stage1.candidates_evaluated,
            provenance=stage1.provenance,
            escalated=True,
            escalation_won=False,
        )

    esc_passed, esc_score, esc_meta = online_verify(esc.patch)
    esc_eval = CandidateResult(
        worker_id=esc.worker_id,
        model=esc.model,
        patch=esc.patch,
        cost_usd=esc.cost_usd,
        wall_s=esc.wall_s,
        score=esc_score,
        passed=esc_passed,
        audit_verified=esc.audit_verified,
        policy_digest=esc.policy_digest,
        meta=esc_meta,
    )

    best = stage1.winner
    escalation_won = False
    composed_tried = stage1.composed_tried
    composed_won = stage1.composed_won
    composed_parents = list(stage1.provenance.get("composed_parents") or [])

    if best is None or (esc_eval.passed, esc_eval.score) > (best.passed, best.score) or (
        not best.passed and not esc_eval.passed and esc_eval.score >= best.score and esc_eval.score > 0.0
    ):
        # When both cheap and escalated patches tie on an imperfect synthetic repro gate (while passing
        # authoritative repo tests, score > 0), prefer the stronger escalated frontier candidate.
        best = esc_eval
        escalation_won = True
        composed_won = False

    # If still not passing, try deterministic 3-way composition between the escalated patch and top cheap patch.
    if not best.passed and stage1.winner is not None and stage1.winner.patch.strip():
        merged, merged_idxs = compose_patches(base_repo, [esc_eval.patch, stage1.winner.patch])
        if merged and hashlib.sha256(merged.encode()).hexdigest() != esc_eval.patch_sha256:
            composed_tried = True
            mp, ms, mm = online_verify(merged)
            if (mp, ms) > (best.passed, best.score):
                composed_parents = [esc_eval.worker_id, stage1.winner.worker_id]
                best = CandidateResult(
                    worker_id="compose(" + "+".join(composed_parents) + ")",
                    model="deterministic-3way",
                    patch=merged,
                    cost_usd=0.0,
                    score=ms,
                    passed=mp,
                    audit_verified=True,
                    policy_digest=esc_eval.policy_digest,
                    meta={"parents": composed_parents, **mm},
                )
                composed_won = True
                escalation_won = True

    prov = dict(stage1.provenance)
    prov["candidates"] = list(prov.get("candidates") or []) + [{
        "worker_id": esc_eval.worker_id,
        "model": esc_eval.model,
        "patch_sha256": esc_eval.patch_sha256,
        "passed": esc_eval.passed,
        "score": esc_eval.score,
        "cost_usd": round(esc_eval.cost_usd, 6),
        "files": sorted(_touched_files(esc_eval.patch)),
        "audit_verified": esc_eval.audit_verified,
        "policy_digest": esc_eval.policy_digest,
    }]
    prov["composed_parents"] = composed_parents
    prov["winner"] = best.worker_id if best else None
    prov["winner_sha256"] = best.patch_sha256 if best else None
    prov["escalated"] = True
    prov["escalation_won"] = escalation_won
    prov["total_cost_usd"] = round(budget.spent_usd, 6)

    return RouteDecision(
        winner=best,
        composed_tried=composed_tried,
        composed_won=composed_won,
        needs_escalation=False,
        total_cost_usd=budget.spent_usd,
        candidates_evaluated=stage1.candidates_evaluated + 1,
        provenance=prov,
        escalated=True,
        escalation_won=escalation_won,
    )
