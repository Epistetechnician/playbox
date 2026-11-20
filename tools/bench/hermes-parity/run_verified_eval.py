#!/usr/bin/env python3
"""Parallel, resumable, zero-oracle SWE-bench evaluation harness for Playbox Attested Gate.

Wraps `run_swe.py` (Clean Arm L / Stock OpenShell Arm S) and `run_swe_playbox_v2.py` /
`hillclimb_playbox.py` (Playbox Arm P2 3-Tier Cascade) with:
1. `--concurrency N` (default 3) using isolated worker slots (`PB_SLOT` / `--worker-slot`),
   isolated Playbox rooms, and isolated OpenShell microVMs per worker.
2. Crash-safe JSONL checkpoint & resume (`<outdir>/checkpoint.jsonl`) plus compounding
   non-oracle gate-verified memory (`<outdir>/verified-memory.jsonl`).
3. Strictly non-oracle 3-Tier Cascade for Arm P2:
   - Tier 1 (`t1`): `gpt-5.6-luna` (`hermes`, `openai-codex`) + `--repro-gate` (~$0.07)
   - Non-oracle cross-family diff critic (`gemini-4-neon-high`, zero tools, zero holdout access)
   - Tier 2 (`t2`): conditional `gemini-4-neon-high` (`devin`) with non-oracle critique
   - Tier 3 (`t3`): conditional 2-worker composition (`claude-opus-5-5-high,gemini-4-neon-high`)
4. Paired statistical analysis (exact McNemar's test, Wilson 95% CIs, Tier 1/2/3 breakdown,
   first-half vs. second-half `verified-memory.jsonl` amortization) and automatic aggregated
   5-Gate `Ed25519+ML-DSA-65` + `T1` compliance bundle export & verification.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import glob
import json
import math
import os
import queue
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent
sys.path.insert(0, str(HERE / "swe"))
sys.path.insert(0, str(REPO_ROOT / "hermes-playbox-backend"))

from hillclimb_playbox import _extract_non_oracle_guidance  # noqa: E402
from playbox_router import get_playbox_tool_version, load_or_create_signing_key  # noqa: E402

RUNNER_P2 = HERE / "run_swe_playbox_v2.py"
RUNNER_LS = HERE / "run_swe.py"
PLAYBOX = os.environ.get("PLAYBOX_BIN", os.path.expanduser("~/.local/bin/playbox"))
DEVIN_BIN = os.environ.get(
    "DEVIN_BIN",
    os.path.expanduser("~/.local/bin/devin")
    if os.path.exists(os.path.expanduser("~/.local/bin/devin"))
    else "/Applications/Devin.app/Contents/Resources/app/extensions/windsurf/devin/bin/devin",
)
SWE_DIR = Path(os.environ.get("PB_SWE_DIR", os.path.expanduser("~/.playbox-hermes/openshell-parity/swe")))
PARITY_DIR = Path(os.environ.get("PB_DIR", os.path.expanduser("~/.playbox-hermes/parity-playbox")))


def discover_instances(spec: str, swe_dir: Path = SWE_DIR) -> list[str]:
    """Resolve `--instances` (`all-cached`, `all-cached-29`, `@file`, or comma-separated IDs)."""
    s = spec.strip()
    if s in ("all-cached", "all-cached-28"):
        out = []
        for p in sorted(glob.glob(str(swe_dir / "django__django-*.inst.json"))):
            out.append(os.path.basename(p).split("__", 1)[1].replace(".inst.json", ""))
        return out
    if s == "all-cached-29":
        out = discover_instances("all-cached", swe_dir)
        if (swe_dir / "pallets__flask-4045.inst.json").is_file():
            out.append("flask")
        return out
    if s == "all-swe-dir":
        out = []
        for p in sorted(glob.glob(str(swe_dir / "*.inst.json"))):
            bn = os.path.basename(p).replace(".inst.json", "")
            if bn == "pallets__flask-4045":
                out.append("flask")
            elif bn.startswith("django__django-"):
                out.append(bn.split("__", 1)[1])
            elif "__" in bn:
                out.append(bn.split("__", 1)[1])
            else:
                out.append(bn)
        # Deduplicate preserving order (e.g. django__django-12700).
        seen: set[str] = set()
        dedup = []
        for x in out:
            if x not in seen:
                seen.add(x)
                dedup.append(x)
        return dedup
    if s.startswith("@"):
        lines = Path(s[1:]).read_text(encoding="utf-8").splitlines()
        return [ln.strip() for ln in lines if ln.strip() and not ln.strip().startswith("#")]
    return [x.strip() for x in s.split(",") if x.strip()]


def wilson_95_ci(successes: int, total: int) -> tuple[float, float]:
    """Wilson score 95% confidence interval `(low, high)` for a binomial proportion."""
    if total <= 0:
        return (0.0, 0.0)
    z = 1.959963984540054
    phat = successes / total
    denom = 1.0 + (z * z) / total
    center = (phat + (z * z) / (2.0 * total)) / denom
    half = (z * math.sqrt((phat * (1.0 - phat) + (z * z) / (4.0 * total)) / total)) / denom
    return (round(max(0.0, center - half), 4), round(min(1.0, center + half), 4))


def exact_mcnemar_test(paired: list[tuple[bool, bool]]) -> dict:
    """Exact binomial McNemar's test on paired `(p2_solved, s_solved)` outcomes.

    `b`: P2 solved & S failed; `c`: P2 failed & S solved.
    """
    n11 = sum(1 for p2, s in paired if p2 and s)
    b = sum(1 for p2, s in paired if p2 and not s)
    c = sum(1 for p2, s in paired if (not p2) and s)
    n00 = sum(1 for p2, s in paired if (not p2) and (not s))
    discordant = b + c
    if discordant == 0:
        p_one = 1.0
        p_two = 1.0
    else:
        denom = float(1 << discordant)
        # One-sided H1: P(P2 > S), i.e. tail k >= b under Binomial(b+c, 0.5).
        p_one = sum(math.comb(discordant, k) for k in range(b, discordant + 1)) / denom
        hi = max(b, c)
        tail = sum(math.comb(discordant, k) for k in range(hi, discordant + 1)) / denom
        p_two = min(1.0, 2.0 * tail)
    return {
        "paired_n": len(paired),
        "both_solved": n11,
        "p2_only_solved_b": b,
        "s_only_solved_c": c,
        "neither_solved": n00,
        "discordant_pairs": discordant,
        "p_value_one_sided_p2_gt_s": round(p_one, 6),
        "p_value_two_sided": round(p_two, 6),
        "significant_alpha_0_05_two_sided": p_two < 0.05,
        "significant_alpha_0_05_one_sided": p_one < 0.05,
    }


def _estimate_devin_critic_cost(ws_dir: Path, t_start: float, model: str) -> float:
    db_path = Path.home() / ".local/share/devin/cli/sessions.db"
    if not db_path.exists():
        return 0.0
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        ws_real = str(ws_dir.resolve())
        rows = conn.execute(
            "SELECT id, working_directory FROM sessions WHERE created_at >= ? ORDER BY created_at DESC LIMIT 80",
            (int(t_start) - 5,),
        ).fetchall()
        sid = None
        for cand_id, wdir_str in rows:
            ws = str(wdir_str or "")
            if str(ws_dir) in ws or ws_real in ws:
                sid = cand_id
                break
        if not sid:
            return 0.0
        seen: set[str] = set()
        inp_total = 0
        c_read_total = 0
        out_total = 0
        for (cm_raw,) in conn.execute("SELECT chat_message FROM message_nodes WHERE session_id=?", (sid,)).fetchall():
            cm = json.loads(cm_raw)
            if cm.get("role") != "assistant":
                continue
            meta = cm.get("metadata") or {}
            req_id = str(meta.get("request_id") or "")
            if not req_id or req_id in seen:
                continue
            seen.add(req_id)
            m = meta.get("metrics") or {}
            inp_total += int(m.get("input_tokens") or 0) + int(m.get("cache_creation_tokens") or 0)
            c_read_total += int(m.get("cache_read_tokens") or 0)
            out_total += int(m.get("output_tokens") or 0)
        uncached = max(0, inp_total - c_read_total)
        ml = model.lower()
        if "opus-5-5" in ml:
            ir, cr, or_ = 4.00, 0.20, 20.00
        elif "neon" in ml:
            ir, cr, or_ = 3.00, 0.15, 15.00
        else:
            ir, cr, or_ = 1.00, 0.10, 5.00
        return round((uncached * ir + c_read_total * cr + out_total * or_) / 1_000_000.0, 6)
    except Exception:  # noqa: BLE001
        return 0.0


def _extract_base_hunk_context(repo_cache_dir: Path, patch_text: str, max_chars: int = 5500) -> str:
    """Extract surrounding lines from the single-commit base repo around touched diff hunks (strictly non-oracle)."""
    if not repo_cache_dir.is_dir() or not patch_text.strip():
        return ""
    chunks: list[str] = []
    cur_file = ""
    for line in patch_text.splitlines():
        m_f = re.match(r"^diff --git a/(.+?) b/", line)
        if m_f:
            cur_file = m_f.group(1)
            continue
        m_h = re.match(r"^@@ -(\d+)(?:,(\d+))? \+", line)
        if m_h and cur_file:
            fp = repo_cache_dir / cur_file
            if not fp.is_file():
                continue
            start_ln = int(m_h.group(1))
            span = int(m_h.group(2) or "10")
            try:
                src_lines = fp.read_text(encoding="utf-8", errors="ignore").splitlines()
            except OSError:
                continue
            lo = max(1, start_ln - 25)
            hi = min(len(src_lines), start_ln + span + 25)
            numbered = "\n".join(f"{idx:4d}| {src_lines[idx - 1]}" for idx in range(lo, hi + 1))
            chunks.append(f"--- Base context: {cur_file} (lines {lo}..{hi}) ---\n{numbered}")
            if sum(len(c) for c in chunks) >= max_chars:
                break
    return ("\n\n".join(chunks))[:max_chars]


def is_devin_authenticated() -> bool:
    if not os.path.exists(DEVIN_BIN):
        return False
    try:
        p = subprocess.run([DEVIN_BIN, "auth", "status"], capture_output=True, text=True, timeout=8)
        out = (p.stdout or "") + (p.stderr or "")
        return p.returncode == 0 and "not logged in" not in out.lower()
    except (OSError, subprocess.SubprocessError):
        return False


def _estimate_hermes_critic_cost(usage_path: Path, model: str) -> float:
    if not usage_path.is_file():
        return 0.0
    try:
        u = json.loads(usage_path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return 0.0
    direct = float(u.get("estimated_cost_usd") or 0.0)
    if direct > 0.0:
        return direct
    inp = int(u.get("input_tokens") or 0)
    cached = int(u.get("cache_read_tokens") or 0)
    out = int(u.get("output_tokens") or 0)
    uncached = max(0, inp - cached)
    ml = model.lower()
    if any(k in ml for k in ("terra", "gpt-6", "sonnet", "neon")):
        ir, cr, or_ = 3.00, 0.30, 15.00
    else:
        ir, cr, or_ = 1.00, 0.10, 5.00
    return round((uncached * ir + cached * cr + out * or_) / 1_000_000.0, 6)


def run_non_oracle_critic(
    *,
    inst: str,
    problem_statement: str,
    repo_cache_dir: Path,
    patch_text: str,
    online_gate_passed: bool,
    existing_tests_passed: bool,
    critic_model: str,
    critic_harness: str = "hermes",
    critic_provider: str = "openai-codex",
) -> tuple[bool, str, float]:
    """Independent non-oracle diff review.

    Anti-Goodhart invariant by type signature: NEVER receives `res`, `holdout`, `test_patch`,
    `FAIL_TO_PASS`, or `PASS_TO_PASS`. Inspects ONLY `problem_statement`, base source context,
    `patch_text`, `existing_tests_passed`, and `online_gate_passed`.
    """
    if not patch_text.strip():
        return False, "Candidate produced an empty patch.", 0.0
    if online_gate_passed:
        return True, "Both existing repository tests and independent non-vacuous repro script passed.", 0.0
    if not existing_tests_passed:
        return False, "Candidate failed both existing repository tests and independent repro gate.", 0.0
    if not critic_model:
        return False, "Partial online gate (score < 1.0) and critic disabled.", 0.0

    base_ctx = _extract_base_hunk_context(repo_cache_dir, patch_text)
    repro_note = (
        "Existing repository unit tests PASSED, and the independent reproduction script PASSED."
        if online_gate_passed
        else "Existing repository unit tests PASSED, while an auto-generated synthetic reproduction script disagreed (which may be a real edge-case miss OR a buggy synthetic repro test)."
    )
    prompt = (
        "You are an adversarial code reviewer gating a software bugfix candidate. "
        "Do NOT run any tools or commands; all needed information is below.\n"
        f"Online check status: {repro_note}\n"
        "Check strictly whether the CANDIDATE PATCH completely and accurately resolves the ISSUE "
        "without regressions:\n"
        "1. Does it patch the right abstraction layer (core helper/iterator vs high-level CLI wrapper so direct unit callers also work)?\n"
        "2. Does it match surrounding codebase conventions (e.g., deprecation warning vs ValueError/TypeError, truthy/bool handling, general iterables tuple/list/set vs single type)?\n"
        "3. Are all requirements and edge cases implied by the ISSUE covered?\n\n"
        "Reply in <= 3 lines:\n"
        "- Line 1 MUST be either `VERDICT: PASS` (if confident the fix is complete and sound) or `VERDICT: ESCALATE` (if there is a real flaw, incomplete edge case, or wrong layer).\n"
        "- Line 2: `CRITIQUE: <concise root-cause critique for the next solver without naming hidden tests>`.\n\n"
        f"=== ISSUE ({inst}) ===\n{problem_statement[:4500]}\n\n"
        f"=== SURROUNDING BASE SOURCE ===\n{base_ctx}\n\n"
        f"=== CANDIDATE PATCH ===\n{patch_text[:6000]}\n"
    )
    with tempfile.TemporaryDirectory(prefix="pbx-critic-") as td:
        if critic_harness == "devin" and os.path.exists(DEVIN_BIN):
            ws = Path(td) / "ws"
            (ws / ".devin").mkdir(parents=True, exist_ok=True)
            (ws / ".devin" / "mcp_config.local.json").write_text(
                json.dumps({"mcpServers": {"playbox": {"disabled": True}}}),
                encoding="utf-8",
            )
            t0 = time.time()
            try:
                p = subprocess.run(
                    [
                        DEVIN_BIN,
                        "--model", critic_model,
                        "--permission-mode", "readonly",
                        "--respect-workspace-trust", "false",
                        "--process-model", "in-process",
                        "--verbosity", "0",
                        "-p", prompt,
                    ],
                    cwd=str(ws),
                    capture_output=True,
                    text=True,
                    timeout=90,
                )
                out = (p.stdout or "").strip()
            except subprocess.TimeoutExpired:
                return online_gate_passed, "Critic timed out.", 0.0
            cost = _estimate_devin_critic_cost(ws, t0, critic_model)
        else:
            usage_p = Path(td) / "critic_usage.json"
            hermes_bin = os.path.expanduser("~/.local/bin/hermes")
            try:
                p = subprocess.run(
                    [
                        hermes_bin,
                        "--usage-file", str(usage_p),
                        "-z", prompt,
                        "-m", critic_model,
                        "--provider", critic_provider,
                        "-t", "",
                        "--yolo",
                    ],
                    cwd=td,
                    capture_output=True,
                    text=True,
                    timeout=90,
                )
                out = (p.stdout or "").strip()
            except subprocess.TimeoutExpired:
                return online_gate_passed, "Critic timed out.", 0.0
            cost = _estimate_hermes_critic_cost(usage_p, critic_model)

        m_verd = re.search(r"VERDICT:\s*(PASS|ESCALATE)", out, re.I)
        verdict_pass = online_gate_passed if not m_verd else (m_verd.group(1).upper() == "PASS")
        m_crit = re.search(r"CRITIQUE:\s*(.+)", out, re.I | re.S)
        critique = m_crit.group(1).strip()[:450] if m_crit else out[-350:]
        return verdict_pass, critique, cost


def _load_checkpoint(ckpt_path: Path) -> dict[tuple[str, str], dict]:
    done: dict[tuple[str, str], dict] = {}
    if not ckpt_path.is_file():
        return done
    for ln in ckpt_path.read_text(encoding="utf-8", errors="ignore").splitlines():
        ln = ln.strip()
        if not ln:
            continue
        try:
            rec = json.loads(ln)
            arm = str(rec.get("arm") or "")
            inst = str(rec.get("inst") or "")
            if arm and inst and rec.get("status") == "completed":
                done[(arm, inst)] = rec
        except ValueError:
            continue
    return done


def _append_checkpoint(ckpt_path: Path, lock: threading.Lock, rec: dict) -> None:
    line = json.dumps(rec, sort_keys=True) + "\n"
    with lock:
        with ckpt_path.open("a", encoding="utf-8") as f:
            f.write(line)
            f.flush()


def _is_provider_failed(usage_obj: dict | None) -> tuple[bool, str]:
    if not isinstance(usage_obj, dict):
        return False, ""
    if usage_obj.get("failed") is True:
        reason = str(usage_obj.get("failure") or "provider_failed")
        return True, reason[:240]
    return False, ""


def run_arm_ls_instance(
    inst: str,
    arm: str,
    slot: int,
    outdir: Path,
    *,
    model: str,
    provider: str,
    timeout: int,
) -> dict:
    env = {
        **os.environ,
        "PB_SLOT": str(slot),
        "PB_MODEL": model,
        "PB_PROVIDER": provider,
        "PB_TIMEOUT": str(timeout),
        "PB_SWE_DIR": str(SWE_DIR),
    }
    subprocess.run(
        [sys.executable, str(RUNNER_LS), inst, arm, "1", str(outdir)],
        env=env,
        check=True,
    )
    tag = f"{inst}-{arm}1"
    res = json.loads((outdir / f"{tag}.json").read_text(encoding="utf-8"))
    u = res.get("usage") or {}
    prov_failed, fail_msg = _is_provider_failed(u)
    cost = float(u.get("estimated_cost_usd") or 0.0)
    return {
        "status": "provider_rate_limited" if prov_failed else "completed",
        "failure": fail_msg,
        "arm": arm,
        "inst": inst,
        "model": model,
        "provider": provider,
        "resolved": bool(res.get("resolved")),
        "f2p": res.get("f2p"),
        "f2p_total": res.get("f2p_total"),
        "p2p": res.get("p2p"),
        "p2p_total": res.get("p2p_total"),
        "wall_s": float(res.get("wall_s") or 0.0),
        "cost_usd": round(cost, 6),
        "patch_lines": res.get("patch_lines", 0),
        "completed_at": time.time(),
    }


def _invoke_p2_tier(
    inst: str,
    tier_label: str,
    slot: int,
    outdir: Path,
    *,
    runtime: str,
    harness: str,
    provider: str,
    model: str,
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
        str(RUNNER_P2),
        inst,
        tier_label,
        str(outdir),
        "--runtime", runtime,
        "--harness", harness,
        "--provider", provider,
        "--model", model,
        "--workers", str(workers),
        "--worker-slot", str(slot),
        "--memory-file", str(memory_file),
        "--memory-gate", "online",
        "--budget", str(budget),
        "--timeout", str(timeout),
    ]
    if worker_models:
        cmd.extend(["--worker-models", worker_models])
    if repro_gate:
        cmd.append("--repro-gate")
    if extra_guidance:
        cmd.extend(["--extra-guidance", extra_guidance])

    env = {**os.environ, "PLAYBOX_BIN": PLAYBOX, "PB_SWE_DIR": str(SWE_DIR)}
    subprocess.run(cmd, env=env, check=True)
    tag = f"{inst}-P2-{tier_label}"
    res = json.loads((outdir / f"{tag}.json").read_text(encoding="utf-8"))
    patch_p = outdir / f"{tag}.patch"
    patch = patch_p.read_text(encoding="utf-8") if patch_p.is_file() else ""
    rcpt_p = outdir / f"{tag}.receipt.json"
    return res, patch, rcpt_p


def run_arm_p2_cascade_instance(
    inst: str,
    slot: int,
    outdir: Path,
    *,
    runtime: str,
    t1_harness: str,
    t1_provider: str,
    t1_model: str,
    t1_repro_gate: bool,
    t2_harness: str,
    t2_provider: str,
    t2_model: str,
    t3_harness: str,
    t3_provider: str,
    t3_workers: int,
    t3_worker_models: str,
    critic_harness: str,
    critic_provider: str,
    critic_model: str,
    max_tiers: int,
    memory_file: Path,
    timeout: int,
) -> dict:
    """Execute the strictly zero-oracle 3-Tier Playbox Arm P2 Cascade on `inst`."""
    from playbox_atlas import append_verified_lesson
    from score import load as load_swe

    r_meta = load_swe(inst)
    problem_statement = str(r_meta.get("problem_statement") or "")
    repo_cache_dir = PARITY_DIR / "swe-repos" / inst

    tiers_run: list[dict] = []
    total_cost_usd = 0.0
    total_wall_s = 0.0
    guidance = ""

    chosen_res: dict | None = None
    chosen_patch = ""
    chosen_tier = "t1"
    chosen_rcpt = ""

    tier_specs = [
        {
            "label": "t1",
            "harness": t1_harness,
            "provider": t1_provider,
            "model": t1_model,
            "workers": 1,
            "worker_models": "",
            "repro_gate": t1_repro_gate,
            "budget": 0.35,
            "timeout": min(timeout, 600),
        },
        {
            "label": "t2",
            "harness": t2_harness,
            "provider": t2_provider,
            "model": t2_model,
            "workers": 1,
            "worker_models": "",
            "repro_gate": False,
            "budget": 1.00,
            "timeout": min(timeout, 660),
        },
        {
            "label": "t3",
            "harness": t3_harness,
            "provider": t3_provider,
            "model": t3_worker_models.split(",")[0].strip() if t3_worker_models else "gpt-6-luna",
            "workers": t3_workers,
            "worker_models": t3_worker_models,
            "repro_gate": False,
            "budget": 1.60,
            "timeout": min(timeout, 780),
        },
    ][: max(1, min(3, max_tiers))]

    for idx, ts in enumerate(tier_specs):
        t_lbl = ts["label"]
        res, patch, rcpt_path = _invoke_p2_tier(
            inst,
            t_lbl,
            slot,
            outdir,
            runtime=runtime,
            harness=ts["harness"],
            provider=ts["provider"],
            model=ts["model"],
            workers=ts["workers"],
            worker_models=ts["worker_models"],
            repro_gate=ts["repro_gate"],
            memory_file=memory_file,
            extra_guidance=guidance,
            budget=ts["budget"],
            timeout=ts["timeout"],
        )
        tier_cost = float(res.get("total_cost_usd") or 0.0)
        tier_wall = float(res.get("wall_s") or 0.0)
        w0_usage = ((res.get("workers_meta") or [{}])[0]).get("usage") or {}
        prov_failed, fail_msg = _is_provider_failed(w0_usage)
        if prov_failed and not patch.strip():
            return {
                "status": "provider_rate_limited",
                "failure": fail_msg,
                "arm": "P2",
                "inst": inst,
                "winning_tier": t_lbl,
                "tiers_executed": len(tiers_run) + 1,
                "resolved": False,
                "cost_usd": round(total_cost_usd + tier_cost, 6),
                "wall_s": round(total_wall_s + tier_wall, 1),
                "completed_at": time.time(),
            }
        online_ok = bool(res.get("online_gate_passed"))
        cands_prov = (res.get("provenance") or {}).get("candidates") or []
        best_score = max((float(c.get("score") or 0.0) for c in cands_prov), default=0.0)
        # `best_score >= 0.4` holds when EITHER existing repo tests passed (0.6) OR independent w_repro passed (0.4,
        # e.g. when changing a global default setting invalidates an un-updated pre-patch test in `tests/`).
        existing_ok = online_ok or (best_score >= 0.4)

        # Strictly non-oracle escalation decision: NEVER inspects `res["resolved"]` or `res["holdout"]`!
        is_last_tier = idx == (len(tier_specs) - 1)
        if is_last_tier:
            critic_ok, critique, critic_cost = (existing_ok and bool(patch.strip())), "final_tier", 0.0
        else:
            t_c0 = time.time()
            critic_ok, critique, critic_cost = run_non_oracle_critic(
                inst=inst,
                problem_statement=problem_statement,
                repo_cache_dir=repo_cache_dir,
                patch_text=patch,
                online_gate_passed=online_ok,
                existing_tests_passed=existing_ok,
                critic_model=critic_model,
                critic_harness=critic_harness,
                critic_provider=critic_provider,
            )
            tier_wall += round(time.time() - t_c0, 1)

        total_cost_usd += tier_cost + critic_cost
        total_wall_s += tier_wall

        tiers_run.append({
            "tier": t_lbl,
            "harness": ts["harness"],
            "model": ts["worker_models"] or ts["model"],
            "online_gate_passed": online_ok,
            "existing_tests_passed": existing_ok,
            "critic_passed": critic_ok,
            "critic_cost_usd": round(critic_cost, 6),
            "tier_cost_usd": round(tier_cost, 6),
            "wall_s": round(tier_wall, 1),
            "has_patch": bool(patch.strip()),
            "receipt_path": str(rcpt_path) if rcpt_path.is_file() else "",
        })

        # Non-oracle candidate selection: prefer latest tier whose patch passed existing repo tests (`existing_ok`),
        # or any non-empty patch over an empty patch.
        prev_cands = ((chosen_res or {}).get("provenance") or {}).get("candidates") or []
        prev_existing_ok = bool(chosen_res and (chosen_res.get("online_gate_passed") or max((float(c.get("score") or 0.0) for c in prev_cands), default=0.0) >= 0.4))
        if chosen_res is None or (bool(patch.strip()) and (existing_ok or not prev_existing_ok)):
            chosen_res = res
            chosen_patch = patch
            chosen_tier = t_lbl
            chosen_rcpt = str(rcpt_path) if rcpt_path.is_file() else ""

        if critic_ok and existing_ok and bool(patch.strip()):
            # Persist verified lesson if non-oracle critic + existing repo tests passed and `run_swe_playbox_v2`
            # hadn't already persisted it (e.g. when synthetic w_repro had a false negative).
            if memory_file and not res.get("lesson_persisted") and res.get("receipt_verified") and patch.strip():
                touched = sorted(set(re.findall(r"(?m)^diff --git a/(.+?) b/", patch)))
                hunks = [ln.strip() for ln in patch.splitlines() if ln.startswith("@@")][:3]
                append_verified_lesson(
                    memory_file,
                    topic=f"{inst} {' '.join(touched)}",
                    lesson=f"Verified fix touched {', '.join(touched)} ({'; '.join(hunks)})",
                    evidence=[f"commit:{str(r_meta.get('base_commit', ''))[:12]}", f"inst:{inst}"],
                    gate_receipt_sha256=str(res.get("receipt_sha256") or ""),
                )
            break

        base_fb = _extract_non_oracle_guidance(res, patch)
        guidance = f"{base_fb} Reviewer critique: {critique}"

    assert chosen_res is not None
    wm = (chosen_res.get("workers_meta") or [{}])[0]
    hold = chosen_res.get("holdout") or {}
    return {
        "status": "completed",
        "arm": "P2",
        "inst": inst,
        "winning_tier": chosen_tier,
        "tiers_executed": len(tiers_run),
        "tiers_trace": tiers_run,
        "resolved": bool(chosen_res.get("resolved")),
        "online_gate_passed": bool(chosen_res.get("online_gate_passed")),
        "f2p": hold.get("f2p"),
        "f2p_total": hold.get("f2p_total"),
        "p2p": hold.get("p2p"),
        "p2p_total": hold.get("p2p_total"),
        "wall_s": round(total_wall_s, 1),
        "winning_tier_wall_s": float(chosen_res.get("wall_s") or 0.0),
        "cost_usd": round(total_cost_usd, 6),
        "winning_tier_cost_usd": float(chosen_res.get("total_cost_usd") or 0.0),
        "median_cmd_s": wm.get("median_cmd_s"),
        "delegates": sum(int(m.get("delegates") or 0) for m in (chosen_res.get("workers_meta") or [])),
        "sync_none": sum(int(m.get("sync_none") or 0) for m in (chosen_res.get("workers_meta") or [])),
        "sync_delta": sum(int(m.get("sync_delta") or 0) for m in (chosen_res.get("workers_meta") or [])),
        "taint_advisories": sum(int(m.get("taint_advisories") or 0) for m in (chosen_res.get("workers_meta") or [])),
        "receipt_verified": bool(chosen_res.get("receipt_verified")),
        "receipt_sha256": chosen_res.get("receipt_sha256", ""),
        "receipt_path": chosen_rcpt,
        "winning_tag": f"{inst}-P2-{chosen_tier}",
        "completed_at": time.time(),
    }


def export_aggregated_5gate_bundle(outdir: Path, p2_records: list[dict]) -> dict:
    """Export and offline-verify a single aggregated 5-Gate `playbox.attested_gate.v1` compliance bundle."""
    receipt_paths = [r["receipt_path"] for r in p2_records if r.get("receipt_path") and Path(r["receipt_path"]).is_file()]
    if not receipt_paths:
        return {"exported": False, "reason": "no_p2_receipts"}

    agg_state = outdir / "agg-gate-state"
    shutil.rmtree(agg_state, ignore_errors=True)
    agg_state.mkdir(parents=True, exist_ok=True)

    # Seed `agg_state` with a verified Playbox room state (`audit.log`, `root.key`, `endpoint.key`) from the run.
    states_root = outdir / "states"
    source_state: Path | None = None
    egress_lines: list[str] = []
    if states_root.is_dir():
        for sd in sorted(states_root.iterdir()):
            if (sd / "audit.log").is_file() and (sd / "root.key").is_file() and (sd / "endpoint.key").is_file():
                if source_state is None:
                    source_state = sd
                if (sd / "egress.jsonl").is_file():
                    egress_lines.extend((sd / "egress.jsonl").read_text(encoding="utf-8").splitlines())

    if source_state is not None:
        for fn in ("audit.log", "root.key", "endpoint.key"):
            shutil.copy2(source_state / fn, agg_state / fn)
    if egress_lines:
        (agg_state / "egress.jsonl").write_text("\n".join(x for x in egress_lines if x.strip()) + "\n", encoding="utf-8")

    key_file = PARITY_DIR / "attested_gate.ed25519"
    load_or_create_signing_key(key_file)
    bundle_out = outdir / "verified-eval-5gate-bundle.json"
    default_attest = Path.home() / ".playbox/history/default.log"

    exp_cmd = [PLAYBOX, "gate", "export", "--sign-key", str(key_file), "--out", str(bundle_out)]
    if source_state is not None:
        exp_cmd.extend(["--state", str(agg_state)])
    for rp in receipt_paths:
        exp_cmd.extend(["--receipt", rp])
    if default_attest.is_file():
        exp_cmd.extend(["--attestation", str(default_attest)])

    exp_proc = subprocess.run(exp_cmd, capture_output=True, text=True, timeout=60)
    if exp_proc.returncode != 0 or not bundle_out.is_file():
        return {
            "exported": False,
            "error": (exp_proc.stderr or exp_proc.stdout)[-500:],
        }

    vfy_cmd = [PLAYBOX, "gate", "verify", str(bundle_out), "--require-signature", "--require-pq", "--json"]
    if default_attest.is_file():
        vfy_cmd.extend(["--require-tier", "t1"])
    vfy_proc = subprocess.run(vfy_cmd, capture_output=True, text=True, timeout=60)
    vfy_json = json.loads(vfy_proc.stdout) if vfy_proc.returncode == 0 and vfy_proc.stdout.strip() else {}
    bundle_doc = json.loads(bundle_out.read_text(encoding="utf-8"))
    tool_ver = str((bundle_doc.get("body") or {}).get("tool_version") or "")
    return {
        "exported": True,
        "verified": bool(vfy_proc.returncode == 0 and vfy_json.get("ok") and vfy_json.get("pq_signed")),
        "bundle_path": str(bundle_out),
        "bundle_digest": vfy_json.get("bundle_digest", bundle_doc.get("bundle_digest", "")),
        "tool_version": tool_ver,
        "git_clean": "+dirty" not in tool_ver,
        "pq_signed": bool(vfy_json.get("pq_signed")),
        "achieved_tier": vfy_json.get("achieved_tier", ""),
        "promotions": vfy_json.get("promotions", 0),
        "room_log_pq_committed": vfy_json.get("room_log_pq_committed", 0),
        "attested_receipts": vfy_json.get("attested_receipts", 0),
        "enforced_gates_count": vfy_json.get("enforced_gates_count", 0),
        "claims_full_84_permf": vfy_json.get("claims_full_84_permf", False),
    }


def summarize_arm(records: list[dict], arm: str) -> dict:
    n = len(records)
    solved = sum(1 for r in records if r.get("resolved"))
    total_cost = round(sum(float(r.get("cost_usd") or 0.0) for r in records), 6)
    walls = [float(r["wall_s"]) for r in records if r.get("wall_s") is not None]
    ci_lo, ci_hi = wilson_95_ci(solved, n)
    out = {
        "arm": arm,
        "instances": n,
        "solved": solved,
        "solve_rate": round(solved / max(n, 1), 4),
        "wilson_95_ci": [ci_lo, ci_hi],
        "total_cost_usd": total_cost,
        "mean_cost_per_task_usd": round(total_cost / max(n, 1), 6) if n else None,
        "mean_cost_per_solve_usd": round(total_cost / solved, 6) if solved else None,
        "mean_wall_s": round(statistics.mean(walls), 1) if walls else None,
        "median_wall_s": round(statistics.median(walls), 1) if walls else None,
    }
    if arm == "P2" and records:
        cmds = [float(r["median_cmd_s"]) for r in records if r.get("median_cmd_s") is not None]
        tier_counts = {"t1": 0, "t2": 0, "t3": 0}
        for r in records:
            wt = str(r.get("winning_tier") or "t1")
            tier_counts[wt] = tier_counts.get(wt, 0) + 1
        # Completion-order split for `verified-memory.jsonl` amortization check.
        ordered = sorted(records, key=lambda r: float(r.get("completed_at") or 0.0))
        mid = len(ordered) // 2
        first_half, second_half = ordered[:mid], ordered[mid:]
        out.update({
            "median_delegate_cmd_s": round(statistics.median(cmds), 3) if cmds else None,
            "tier_finish_counts": tier_counts,
            "tier1_finish_rate": round(tier_counts.get("t1", 0) / max(n, 1), 4),
            "all_receipts_verified": all(bool(r.get("receipt_verified")) for r in records),
            "zero_taint_advisories": all(int(r.get("taint_advisories") or 0) == 0 for r in records),
            "memory_amortization": {
                "first_half_n": len(first_half),
                "first_half_mean_cost_usd": round(statistics.mean(float(x.get("cost_usd") or 0.0) for x in first_half), 6) if first_half else None,
                "first_half_mean_wall_s": round(statistics.mean(float(x.get("wall_s") or 0.0) for x in first_half), 1) if first_half else None,
                "second_half_n": len(second_half),
                "second_half_mean_cost_usd": round(statistics.mean(float(x.get("cost_usd") or 0.0) for x in second_half), 6) if second_half else None,
                "second_half_mean_wall_s": round(statistics.mean(float(x.get("wall_s") or 0.0) for x in second_half), 1) if second_half else None,
            },
        })
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--outdir", default="/tmp/swe-verified-eval")
    ap.add_argument("--instances", default="all-cached")
    ap.add_argument("--arms", default="S,P2", help="Comma-separated arms to evaluate (e.g. S,P2 or L,S,P2)")
    ap.add_argument("--concurrency", type=int, default=3, help="Number of isolated concurrent worker slots")
    ap.add_argument("--resume", dest="resume", action="store_true", default=True)
    ap.add_argument("--no-resume", dest="resume", action="store_false")
    ap.add_argument("--runtime", default="openshell", choices=["openshell", "libkrun", "docker"])
    ap.add_argument("--arm-s-model", default="claude-haiku-5-5")
    ap.add_argument("--arm-s-provider", default="anthropic")
    ap.add_argument("--t1-harness", default="hermes", choices=["hermes", "devin"])
    ap.add_argument("--t1-provider", default="anthropic")
    ap.add_argument("--t1-model", default="claude-haiku-5-5")
    ap.add_argument("--no-t1-repro-gate", action="store_true")
    ap.add_argument("--t2-harness", default="devin", choices=["hermes", "devin"])
    ap.add_argument("--t2-provider", default="anthropic")
    ap.add_argument("--t2-model", default="gemini-4-neon-high")
    ap.add_argument("--t3-harness", default="devin", choices=["hermes", "devin"])
    ap.add_argument("--t3-provider", default="anthropic")
    ap.add_argument("--t3-workers", type=int, default=2)
    ap.add_argument("--t3-worker-models", default="claude-opus-5-5-high,gemini-4-neon-high")
    ap.add_argument("--critic-harness", default="devin", choices=["hermes", "devin"])
    ap.add_argument("--critic-provider", default="anthropic")
    ap.add_argument("--critic-model", default="gemini-4-neon-high", help="Independent non-oracle diff critic model (empty to disable)")
    ap.add_argument("--max-tiers", type=int, default=3, choices=[1, 2, 3])
    ap.add_argument("--timeout", type=int, default=780)
    args = ap.parse_args()

    devin_ok = is_devin_authenticated()
    auth_fallback = ""
    t2_harness, t2_provider, t2_model = args.t2_harness, args.t2_provider, args.t2_model
    t3_harness, t3_provider, t3_worker_models = args.t3_harness, args.t3_provider, args.t3_worker_models
    critic_harness, critic_provider, critic_model = args.critic_harness, args.critic_provider, args.critic_model
    if not devin_ok:
        if t2_harness == "devin":
            t2_harness, t2_provider, t2_model = "hermes", "anthropic", "claude-sonnet-5"
            auth_fallback = "hermes:anthropic (claude-haiku-5-5 -> claude-sonnet-5 -> claude-sonnet-5-5,claude-sonnet-5)"
        if t3_harness == "devin":
            t3_harness, t3_provider, t3_worker_models = "hermes", "anthropic", "claude-sonnet-5-5,claude-sonnet-5"
        if critic_harness == "devin" and critic_model:
            critic_harness, critic_provider, critic_model = "hermes", "anthropic", "claude-sonnet-5"

    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)
    ckpt_path = outdir / "checkpoint.jsonl"
    memory_file = outdir / "verified-memory.jsonl"
    load_or_create_signing_key(PARITY_DIR / "attested_gate.ed25519")

    instances = discover_instances(args.instances)
    arms = [a.strip().upper() for a in args.arms.split(",") if a.strip()]
    completed = _load_checkpoint(ckpt_path) if args.resume else {}
    ckpt_lock = threading.Lock()

    tool_ver = get_playbox_tool_version()
    print(json.dumps({
        "event": "eval_start",
        "tool_version": tool_ver,
        "git_clean": "+dirty" not in tool_ver,
        "devin_authenticated": devin_ok,
        "auth_fallback": auth_fallback,
        "instances": len(instances),
        "arms": arms,
        "concurrency": args.concurrency,
        "resumed_records": len(completed),
    }), flush=True)

    t_start_all = time.time()
    for arm in arms:
        todo = [inst for inst in instances if (arm, inst) not in completed]
        if not todo:
            continue
        slot_q: queue.Queue[int] = queue.Queue()
        for s in range(max(1, args.concurrency)):
            slot_q.put(s)

        def _worker(inst_id: str) -> dict:
            slot = slot_q.get()
            try:
                if arm in ("L", "S"):
                    rec = run_arm_ls_instance(
                        inst_id,
                        arm,
                        slot,
                        outdir,
                        model=args.arm_s_model,
                        provider=args.arm_s_provider,
                        timeout=args.timeout,
                    )
                elif arm == "P2":
                    rec = run_arm_p2_cascade_instance(
                        inst_id,
                        slot,
                        outdir,
                        runtime=args.runtime,
                        t1_harness=args.t1_harness,
                        t1_provider=args.t1_provider,
                        t1_model=args.t1_model,
                        t1_repro_gate=not args.no_t1_repro_gate,
                        t2_harness=t2_harness,
                        t2_provider=t2_provider,
                        t2_model=t2_model,
                        t3_harness=t3_harness,
                        t3_provider=t3_provider,
                        t3_workers=args.t3_workers,
                        t3_worker_models=t3_worker_models,
                        critic_harness=critic_harness,
                        critic_provider=critic_provider,
                        critic_model=critic_model,
                        max_tiers=args.max_tiers,
                        memory_file=memory_file,
                        timeout=args.timeout,
                    )
                else:
                    raise ValueError(f"unsupported arm {arm!r}")
                _append_checkpoint(ckpt_path, ckpt_lock, rec)
                print(json.dumps({
                    "event": "instance_done",
                    "arm": arm,
                    "inst": inst_id,
                    "slot": slot,
                    "resolved": rec.get("resolved"),
                    "winning_tier": rec.get("winning_tier", "-"),
                    "cost_usd": rec.get("cost_usd"),
                    "wall_s": rec.get("wall_s"),
                }), flush=True)
                return rec
            finally:
                slot_q.put(slot)

        with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.concurrency)) as pool:
            futs = {pool.submit(_worker, inst): inst for inst in todo}
            for fut in concurrent.futures.as_completed(futs):
                inst_id = futs[fut]
                rec = fut.result()
                if rec.get("status") == "completed":
                    completed[(arm, inst_id)] = rec

    arm_summaries: dict[str, dict] = {}
    arm_records: dict[str, list[dict]] = {}
    for arm in arms:
        recs = [completed[(arm, inst)] for inst in instances if (arm, inst) in completed]
        arm_records[arm] = recs
        arm_summaries[arm] = summarize_arm(recs, arm)

    mcnemar_p2_vs_s = None
    if "P2" in arm_records and "S" in arm_records:
        s_map = {r["inst"]: bool(r.get("resolved")) for r in arm_records["S"]}
        p2_map = {r["inst"]: bool(r.get("resolved")) for r in arm_records["P2"]}
        paired = [(p2_map[i], s_map[i]) for i in instances if i in p2_map and i in s_map]
        mcnemar_p2_vs_s = exact_mcnemar_test(paired)

    gate_bundle_info = (
        export_aggregated_5gate_bundle(outdir, arm_records.get("P2", []))
        if "P2" in arm_records
        else {"exported": False}
    )

    summary = {
        "schema": "playbox.verified_eval_summary.v1",
        "tool_version": tool_ver,
        "git_clean": "+dirty" not in tool_ver,
        "concurrency": args.concurrency,
        "instances_count": len(instances),
        "instances": instances,
        "total_eval_wall_s": round(time.time() - t_start_all, 1),
        "verified_lessons_count": len(memory_file.read_text(encoding="utf-8").splitlines()) if memory_file.exists() else 0,
        "arms": arm_summaries,
        "mcnemar_p2_vs_s": mcnemar_p2_vs_s,
        "gate_5_compliance_bundle": gate_bundle_info,
        "per_instance": {
            inst: {arm: completed.get((arm, inst)) for arm in arms if (arm, inst) in completed}
            for inst in instances
        },
    }
    sum_path = outdir / "verified-eval-summary.json"
    sum_path.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps({
        "event": "eval_complete",
        "summary_path": str(sum_path),
        "tool_version": tool_ver,
        "git_clean": summary["git_clean"],
        "arms": arm_summaries,
        "mcnemar_p2_vs_s": mcnemar_p2_vs_s,
        "gate_5_compliance_bundle": gate_bundle_info,
    }), flush=True)


if __name__ == "__main__":
    main()
