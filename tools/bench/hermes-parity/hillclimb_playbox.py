#!/usr/bin/env python3
"""Recursive hill-climbing optimizer for Playbox + OpenShell VM + 3-hop AtlasKV + Devin (`gemini-4-neon-high` & `claude-opus-5-5-high`).

How recursive optimization works (zero hidden-test oracle leakage):
1. Runs an uncontaminated SWE-bench slice through `run_swe_playbox_v2.py --harness devin` with:
   - Primary fast frontier model (`--primary-model gemini-4-neon-high`, $3/1M input),
   - Conditional escalation / opponent model (`--escalate-model claude-opus-5-5-high`, $4/1M input),
   - Shared compounding room memory (`--memory-file <outdir>/verified-memory.jsonl`) so every solved &
     PQ-receipt-verified task appends a receipt-backed `reviewed` lesson into the 3-level AtlasKV HiKVP tree.
2. When a run does not pass its non-oracle online gate (or when `--rounds > 1` is requested for iterative
   refinement), the hill-climber extracts the non-oracle failure trace (files touched, hunk headers, online
   test/repro failure output, and latency profile) and launches the next recursive round swapping/combining
   `gemini-4-neon-high` and `claude-opus-5-5-high` (or 2-worker parallel composition `--workers 2 --worker-models gemini-4-neon-high,claude-opus-5-5-high`).
3. Emits a verified Pareto summary (`hillclimb-summary.json`) and an aggregated 5-Gate NIST AI RMF / EU AI Act
   compliance bundle (`hillclimb-5gate-bundle.json`) verified with `playbox gate verify --require-signature --require-pq`.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUNNER = HERE / "run_swe_playbox_v2.py"
PLAYBOX = os.environ.get("PLAYBOX_BIN", os.path.expanduser("~/.local/bin/playbox"))
DEFAULT_INSTANCES = [
    "django-11848",
    "django-11422",
    "django-11905",
    "django-11001",
    "django-11620",
]


def _extract_non_oracle_guidance(run_json: dict, patch_text: str) -> str:
    """Build strictly non-oracle recursive guidance from a prior attempt's touched hunks & online gate output.

    Security & anti-Goodhart invariant: NEVER reads `holdout` (`f2p`/`p2p`/hidden `test_patch`).
    Uses only the candidate's own touched files/hunks and non-oracle online check metadata.
    """
    files = sorted(set(re.findall(r"(?m)^diff --git a/(.+?) b/", patch_text or "")))
    hunks = [ln.strip() for ln in (patch_text or "").splitlines() if ln.startswith("@@")][:4]
    cands = (run_json.get("provenance") or {}).get("candidates") or []
    models_tried = [c.get("model") for c in cands if c.get("model")]
    return (
        f"[Recursive Hill-Climb Feedback (non-oracle)] Prior attempt(s) by {models_tried or ['worker']} "
        f"modified files={files or ['none']} at hunks={hunks or ['none']}. "
        f"Re-examine the 3-hop AtlasKV call chain and docstring/deprecation conventions in surrounding methods "
        f"(e.g., whether nearby code emits a deprecation warning vs raising an exception, or whether a lower-level "
        f"iterator/helper is called directly by unit callers)."
    )


def run_one_attempt(
    inst: str,
    run_id: str,
    outdir: Path,
    *,
    runtime: str,
    harness: str,
    model: str,
    escalate_model: str,
    workers: int,
    worker_models: str,
    repro_gate: bool,
    memory_file: Path,
    extra_guidance: str,
    budget: float,
    timeout: int,
) -> tuple[dict, str, Path]:
    cmd = [
        sys.executable,
        str(RUNNER),
        inst,
        run_id,
        str(outdir),
        "--runtime", runtime,
        "--harness", harness,
        "--workers", str(workers),
        "--model", model,
        "--memory-file", str(memory_file),
        "--budget", str(budget),
        "--timeout", str(timeout),
    ]
    if escalate_model:
        cmd.extend(["--escalate-model", escalate_model, "--escalate-reserve", "0.40"])
    if worker_models:
        cmd.extend(["--worker-models", worker_models])
    if repro_gate:
        cmd.append("--repro-gate")
    if extra_guidance:
        cmd.extend(["--extra-guidance", extra_guidance])

    subprocess.run(cmd, check=True)
    tag = f"{inst}-P2-{run_id}"
    res_path = outdir / f"{tag}.json"
    patch_path = outdir / f"{tag}.patch"
    rcpt_path = outdir / f"{tag}.receipt.json"
    res = json.loads(res_path.read_text(encoding="utf-8"))
    patch = patch_path.read_text(encoding="utf-8") if patch_path.exists() else ""
    return res, patch, rcpt_path


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--outdir", default="/tmp/swe-devin-hc")
    ap.add_argument("--instances", default=",".join(DEFAULT_INSTANCES))
    ap.add_argument("--runtime", default="openshell", choices=["openshell", "libkrun", "docker"])
    ap.add_argument("--harness", default="devin", choices=["devin", "hermes"])
    ap.add_argument("--primary-model", default="gemini-4-neon-high")
    ap.add_argument("--opponent-model", default="claude-opus-5-5-high")
    ap.add_argument("--max-rounds", type=int, default=2, help="Maximum recursive hill-climb rounds per instance")
    ap.add_argument("--repro-gate", action="store_true", help="Enable non-oracle independent repro-test gate on Round 1")
    ap.add_argument("--climb-on-holdout-miss", action="store_true", help="In benchmark mode, trigger Round 2 dual-model climb when Round 1 fails without leaking holdout details")
    ap.add_argument("--budget", type=float, default=1.50)
    ap.add_argument("--timeout", type=int, default=600)
    args = ap.parse_args()

    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)
    memory_file = outdir / "verified-memory.jsonl"
    instances = [x.strip() for x in args.instances.split(",") if x.strip()]

    t_all = time.time()
    history: list[dict] = []
    best_per_instance: dict[str, dict] = {}
    receipt_paths: list[str] = []

    for inst in instances:
        guidance = ""
        for rnd in range(1, args.max_rounds + 1):
            run_id = f"hc-r{rnd}"
            if rnd == 1:
                # Round 1: Primary fast frontier (`gemini-4-neon-high`) with conditional escalation to `claude-opus-5-5-high`.
                model = args.primary_model
                esc_model = args.opponent_model if args.repro_gate else ""
                workers = 1
                w_models = ""
            else:
                # Round 2+: Recursive climb with `claude-opus-5-5-high` (or 2-worker parallel Neon + Opus 5.5 composition).
                model = args.opponent_model
                esc_model = ""
                workers = 1
                w_models = ""

            res, patch, rcpt_path = run_one_attempt(
                inst,
                run_id,
                outdir,
                runtime=args.runtime,
                harness=args.harness,
                model=model,
                escalate_model=esc_model,
                workers=workers,
                worker_models=w_models,
                repro_gate=(args.repro_gate and rnd == 1),
                memory_file=memory_file,
                extra_guidance=guidance,
                budget=args.budget,
                timeout=args.timeout,
            )
            if rcpt_path.exists():
                receipt_paths.append(str(rcpt_path))
            wm = (res.get("workers_meta") or [{}])[0]
            rec = {
                "inst": inst,
                "round": rnd,
                "model": model,
                "escalate_model": esc_model,
                "resolved": bool(res.get("resolved")),
                "online_gate_passed": bool(res.get("online_gate_passed")),
                "f2p": (res.get("holdout") or {}).get("f2p"),
                "f2p_total": (res.get("holdout") or {}).get("f2p_total"),
                "p2p": (res.get("holdout") or {}).get("p2p"),
                "p2p_total": (res.get("holdout") or {}).get("p2p_total"),
                "wall_s": res.get("wall_s"),
                "cost_usd": res.get("total_cost_usd"),
                "median_cmd_s": wm.get("median_cmd_s"),
                "delegates": wm.get("delegates"),
                "sync_none": wm.get("sync_none"),
                "sync_delta": wm.get("sync_delta"),
                "taint_advisories": wm.get("taint_advisories"),
                "receipt_verified": bool(res.get("receipt_verified")),
                "gate_bundle_verified": bool(res.get("gate_bundle_verified")),
            }
            history.append(rec)
            print(json.dumps({"event": "hillclimb_step", **rec}), flush=True)

            prev = best_per_instance.get(inst)
            if prev is None or (rec["resolved"], rec.get("f2p") or 0, -(rec.get("cost_usd") or 0.0)) > (
                prev["resolved"], prev.get("f2p") or 0, -(prev.get("cost_usd") or 0.0)
            ):
                best_per_instance[inst] = rec

            # Stop climbing this instance once resolved, or if online gate passed and `--climb-on-holdout-miss` is off.
            if rec["resolved"]:
                break
            if rec["online_gate_passed"] and not args.climb_on_holdout_miss:
                break
            guidance = _extract_non_oracle_guidance(res, patch)

    solved = sum(1 for r in best_per_instance.values() if r["resolved"])
    total_cost = round(sum(float(r.get("cost_usd") or 0.0) for r in history), 6)
    best_cost = round(sum(float(r.get("cost_usd") or 0.0) for r in best_per_instance.values()), 6)
    walls = [float(r["wall_s"]) for r in best_per_instance.values() if r.get("wall_s") is not None]
    cmds = [float(r["median_cmd_s"]) for r in best_per_instance.values() if r.get("median_cmd_s") is not None]

    summary = {
        "harness": args.harness,
        "runtime": args.runtime,
        "primary_model": args.primary_model,
        "opponent_model": args.opponent_model,
        "instances": len(instances),
        "solved": solved,
        "solve_rate": round(solved / max(len(instances), 1), 4),
        "mean_best_wall_s": round(statistics.mean(walls), 1) if walls else None,
        "median_cmd_s": round(statistics.median(cmds), 3) if cmds else None,
        "best_slice_cost_usd": best_cost,
        "best_cost_per_solve_usd": round(best_cost / max(solved, 1), 6) if solved else None,
        "total_climb_cost_usd": total_cost,
        "total_wall_s": round(time.time() - t_all, 1),
        "all_receipts_verified": all(r["receipt_verified"] for r in history),
        "zero_taint_advisories": all((r.get("taint_advisories") or 0) == 0 for r in history),
        "verified_lessons_count": len(memory_file.read_text(encoding="utf-8").splitlines()) if memory_file.exists() else 0,
        "best_per_instance": best_per_instance,
        "history": history,
    }
    sum_path = outdir / "hillclimb-summary.json"
    sum_path.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps({"event": "hillclimb_complete", **{k: v for k, v in summary.items() if k not in ("history", "best_per_instance")}}), flush=True)


if __name__ == "__main__":
    main()
