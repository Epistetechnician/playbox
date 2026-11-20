#!/usr/bin/env python3
"""Arm P2 of the SWE parity harness: stateful Playbox+OpenShell/libkrun+Hermes + 3-hop AtlasKV
+ non-oracle online gate + conditional frontier cascade + Ed25519 Attested Gate Receipt.

Usage:
  run_swe_playbox_v2.py INST(django-NNNNN) N OUTDIR
    [--runtime openshell|libkrun|docker]
    [--workers 1] [--provider anthropic] [--model claude-haiku-4-5-20251001]
    [--escalate-model claude-sonnet-4-6] [--escalate-provider anthropic] [--escalate-reserve 0.35]
    [--repro-gate] [--memory-file PATH] [--budget 0.60] [--no-atlas] [--legacy-replay]

Anti-Goodhart & security invariants:
1. Single-commit `git archive` base repo verified by `verify_single_commit_repo()` + `.git` stripped
   inside `/sandbox` — closes `git log --all` future-commit reward-hacking by construction.
2. Online gating (`online_verify`) runs ONLY existing repo tests + optional independent non-vacuous
   reproduction test (`--repro-gate`, generated without seeing any candidate patch). Hidden SWE-bench
   `test_patch` is NEVER exposed to any worker, repro generator, or escalation prompt.
3. Every run emits an Ed25519-signed `playbox.attested_gate_receipt.v1` binding isolation proofs,
   host `audit.log` verification + SHA-256, taint stats, AtlasKV provenance, router cascade, and
   final sealed holdout score.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

H = Path(__file__).resolve().parent
REPO_ROOT = H.parent.parent.parent
sys.path.insert(0, str(H / "swe"))
sys.path.insert(0, str(REPO_ROOT / "hermes-playbox-backend"))

spec = importlib.util.spec_from_file_location("pbrun", H.parent / "hermes-parity-playbox" / "run.py")
R = importlib.util.module_from_spec(spec)
spec.loader.exec_module(R)

from playbox_atlas import (  # noqa: E402
    HiKVPIndex,
    append_verified_lesson,
    build_repo_triples,
    format_brief,
    load_room_memory_jsonl,
)
from playbox_router import (  # noqa: E402
    BudgetCap,
    CandidateResult,
    ReproGateResult,
    build_signed_receipt,
    is_genuine_repro_failure,
    load_or_create_signing_key,
    run_cascade,
    verify_signed_receipt,
    verify_single_commit_repo,
)
from score import load, score  # noqa: E402

IMG = os.environ.get("PB_SWE_IMAGE", "pb-swe-django:arm64")
SWE = os.path.expanduser("~/.playbox-hermes/openshell-parity/swe")
GIT = ["git", "-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0", "-c", "user.name=bench", "-c", "user.email=bench@example.com"]


def _start_host_with_runtime(
    d: str,
    runtime: str,
    *,
    egress_file: str = "",
    allow_egress: list[str] | None = None,
):
    """Like `R.Host`, parameterized by `--runtime` (`libkrun`, `docker`, or `openshell`) and optional BOX-7 egress."""
    import threading

    class _CustomHost(R.Host):
        def __init__(self, d_path: str):
            self.d = d_path
            self.home = os.path.join(d_path, "phome")
            os.makedirs(self.home)
            self.hosting = os.path.join(d_path, "hosting")
            os.makedirs(self.hosting)
            self.env = {
                **os.environ,
                "PLAYBOX_HOME": self.home,
                "PLAYBOX_HOSTING_DIR": self.hosting,
                "PLAYBOX_ALLOW_MISSING_LIMITS": "1",
            }
            argv = [
                R.PLAYBOX, "up", "--home", self.home, "--local", "--console",
                "--state", os.path.join(d_path, "state"),
                "--repo", os.path.join(d_path, "repo"),
                "--runtime", runtime, "--allow-image", R.IMAGE,
                "--grant-ttl", "43200", "--max-tasks-per-minute", "300",
            ]
            if egress_file:
                argv.extend(["--egress", egress_file])
            for eg in allow_egress or []:
                argv.extend(["--allow-egress", eg])
            self.p = subprocess.Popen(
                argv,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=self.env,
            )
            self.links = []
            threading.Thread(target=self._pump, daemon=True).start()
            for _ in range(60):
                if self.links:
                    break
                time.sleep(0.5)
            else:
                raise RuntimeError(f"playbox host ({runtime}) did not come up")
            for _ in range(5):
                self.cmd("link")
                time.sleep(1.0)
            self._reg = None

    return _CustomHost(d)


def host_repo(dst: str, inst: str, r: dict) -> None:
    cache = os.path.join(R.D, "swe-repos", inst)
    if not os.path.isdir(os.path.join(cache, ".git")):
        os.makedirs(cache, exist_ok=True)
        tb = f"{SWE}/django/testbed"
        subprocess.run(f"git -C {tb} archive {r['base_commit']} | tar -x -C {cache}", shell=True, check=True)
        subprocess.run([*GIT, "init", "-q", "-b", "main", cache], check=True)
        subprocess.run([*GIT, "-C", cache, "add", "-A"], check=True)
        subprocess.run([*GIT, "-C", cache, "commit", "-q", "-m", "base"], check=True)
    shutil.copytree(cache, dst, symlinks=True)


def strip_tests(patch: str) -> str:
    blocks = re.split(r"(?m)^(?=diff --git )", patch or "")
    return "".join(b for b in blocks if b.strip() and not re.match(r"diff --git a/tests/", b))


def estimate_usage_cost_usd(u: dict, model: str = "") -> float:
    """Return provider-reported USD cost, or token-metered API-equivalent USD when running on
    subscription OAuth (`cost_status == 'included'`, where provider reports 0.0)."""
    direct = float(u.get("estimated_cost_usd") or 0.0)
    if direct > 0.0:
        return direct
    inp = int(u.get("input_tokens") or 0)
    cached = int(u.get("cache_read_tokens") or 0)
    out = int(u.get("output_tokens") or 0)
    if inp == 0 and out == 0:
        return 0.0
    uncached = max(0, inp - cached)
    m = (model or str(u.get("model") or "")).lower()
    if any(k in m for k in ("sonnet", "terra", "opus", "gpt-5.4")):
        in_rate, cache_rate, out_rate = 3.00, 0.30, 15.00
    else:
        in_rate, cache_rate, out_rate = 1.00, 0.10, 5.00
    return round((uncached * in_rate + cached * cache_rate + out * out_rate) / 1_000_000.0, 6)


def build_atlas_brief(
    repo_dir: str,
    issue_text: str,
    base_commit: str,
    *,
    index_out: str | Path | None = None,
    memory_file: str | Path | None = None,
) -> tuple[str, dict]:
    t0 = time.time()
    triples = build_repo_triples(repo_dir, base_commit=base_commit)
    if memory_file:
        triples.extend(load_room_memory_jsonl(memory_file))
    idx = HiKVPIndex.build(triples)
    if index_out:
        idx.save(index_out)
    hits = idx.query(issue_text, top_root=5, top_inter=12, top_leaf=14, expand_hops=3, allow_untrusted=False)
    brief = format_brief(hits, max_chars=3200)
    return brief, {"build_and_query_s": round(time.time() - t0, 3), "hits": len(hits), "hops": 3, **idx.stats()}


def _prepare_worker_home(hh: str, *, enable_atlas_plugin: bool) -> None:
    R.make_home(hh, "P", str(REPO_ROOT / "hermes-playbox-backend" / "playbox_terminal"))
    real_auth = os.path.expanduser("~/.hermes/auth.json")
    hh_auth = os.path.join(hh, "auth.json")
    if os.path.exists(real_auth) and not os.path.islink(hh_auth):
        try:
            os.remove(hh_auth)
        except OSError:
            pass
        os.symlink(real_auth, hh_auth)
    atlas_link = os.path.join(hh, "plugins", "playbox-atlas")
    if os.path.islink(atlas_link):
        os.remove(atlas_link)
    if enable_atlas_plugin:
        os.symlink(str(REPO_ROOT / "hermes-playbox-backend" / "playbox_atlas"), atlas_link)
        subprocess.run(
            [
                R.HVENV_PY, "-c",
                "import yaml,sys; p=sys.argv[1]; c=yaml.safe_load(open(p)); "
                "e=c.setdefault('plugins',{}).setdefault('enabled',[]); "
                "('playbox-atlas' not in e) and e.append('playbox-atlas'); "
                "open(p,'w').write(yaml.safe_dump(c))",
                os.path.join(hh, "config.yaml"),
            ],
            check=True,
        )


def _run_sealed_container_check(
    r: dict,
    patch: str,
    *,
    test_mod: str,
    repro_script: str = "",
) -> tuple[bool, float, dict]:
    """Non-oracle online gate inside a sealed no-network container.

    Runs existing repository tests (`test_mod`, weight 0.6) and optional independent reproduction
    script (`repro_script`, weight 0.4) separately so passing the authoritative repo test suite
    is scored distinctly from a synthetic repro failure. Never touches hidden SWE-bench `test_patch`.
    """
    with tempfile.TemporaryDirectory(prefix="pbx-online-gate-") as td:
        Path(td, "agent.patch").write_text(patch or "", encoding="utf-8")
        if repro_script.strip():
            Path(td, "repro.py").write_text(repro_script, encoding="utf-8")
        apply_step = "git apply /s/agent.patch && " if (patch or "").strip() else ""
        has_repro = bool(repro_script.strip())
        if has_repro:
            sh = (
                f"mkdir -p /tmp/tb && git -C /usr/local/testbed archive {r['base_commit']} | tar -x -C /tmp/tb "
                f"&& cd /tmp/tb && {apply_step}"
                f"(cd tests && PYTHONPATH=/tmp/tb /usr/local/venv/bin/python runtests.py --settings=test_sqlite --parallel 1 {test_mod}); "
                f"__EX=$?; echo ___PBX_EXISTING_RC=$__EX; "
                f"PYTHONPATH=/tmp/tb /usr/local/venv/bin/python /s/repro.py; "
                f"__RP=$?; echo ___PBX_REPRO_RC=$__RP"
            )
        else:
            sh = (
                f"mkdir -p /tmp/tb && git -C /usr/local/testbed archive {r['base_commit']} | tar -x -C /tmp/tb "
                f"&& cd /tmp/tb && {apply_step}"
                f"(cd tests && PYTHONPATH=/tmp/tb /usr/local/venv/bin/python runtests.py --settings=test_sqlite --parallel 1 {test_mod}); "
                f"__EX=$?; echo ___PBX_EXISTING_RC=$__EX"
            )
        p = subprocess.run(
            ["docker", "run", "--rm", "--network", "none", "-v", f"{td}:/s:ro", "--entrypoint", "bash", IMG, "-c", sh],
            capture_output=True,
            text=True,
            timeout=600,
        )
        out = (p.stdout or "") + "\n" + (p.stderr or "")
        m_ex = re.search(r"___PBX_EXISTING_RC=(\d+)", out)
        m_rp = re.search(r"___PBX_REPRO_RC=(\d+)", out)
        existing_ok = bool(m_ex and int(m_ex.group(1)) == 0)
        repro_ok = (not has_repro) or bool(m_rp and int(m_rp.group(1)) == 0)
        passed = existing_ok and repro_ok
        if has_repro:
            score_val = (0.6 if existing_ok else 0.0) + (0.4 if repro_ok else 0.0)
        else:
            score_val = 1.0 if existing_ok else 0.0
        return passed, round(score_val, 3), {
            "existing_ok": existing_ok,
            "repro_ok": repro_ok,
            "online_failure": "" if passed else out[-600:],
            "repro_checked": has_repro,
        }


def _check_repro_fails_on_base(r: dict, repro_script: str) -> tuple[bool, bool, str, str]:
    """Anti-vacuous + sanity check: independent repro script MUST exit non-zero on unpatched `base_commit`
    due to a genuine behavioral/assertion failure (never `SyntaxError`, `ImportError`, `ModuleNotFoundError`, `NameError`)."""
    if not repro_script.strip():
        return False, False, "empty repro script", "empty repro script"
    ok_ast, ast_reason = is_genuine_repro_failure("", repro_script)
    if not ok_ast:
        return False, False, ast_reason, ast_reason
    with tempfile.TemporaryDirectory(prefix="pbx-repro-base-") as td:
        Path(td, "repro.py").write_text(repro_script, encoding="utf-8")
        sh = (
            f"mkdir -p /tmp/tb && git -C /usr/local/testbed archive {r['base_commit']} | tar -x -C /tmp/tb "
            f"&& cd /tmp/tb && PYTHONPATH=/tmp/tb /usr/local/venv/bin/python /s/repro.py"
        )
        p = subprocess.run(
            ["docker", "run", "--rm", "--network", "none", "-v", f"{td}:/s:ro", "--entrypoint", "bash", IMG, "-c", sh],
            capture_output=True,
            text=True,
            timeout=180,
        )
        out = ((p.stdout or "") + "\n" + (p.stderr or "")).strip()[-800:]
        fails = p.returncode != 0
        if not fails:
            return False, True, "repro passed on unpatched base_commit (vacuous)", out
        sanity_ok, reason = is_genuine_repro_failure(out, repro_script)
        return fails, sanity_ok, ("" if sanity_ok else reason), out


def run_repro_generator(
    *,
    r: dict,
    run_dir: str,
    host,
    link: str,
    provider: str,
    model: str,
    atlas_brief: str,
    atlas_index_path: str,
    timeout: int,
    keep: bool,
    approvers_hex: str = "",
) -> ReproGateResult:
    """Run an independent cheap worker that sees ONLY the issue + AtlasKV (never any solver patch)
    and writes a standalone reproduction script `/sandbox/_pbx_repro.py` that fails on base_commit."""
    wdir = os.path.join(run_dir, "w_repro")
    os.makedirs(wdir, exist_ok=True)
    hh = os.path.join(R.D, "home-P2-repro")
    _prepare_worker_home(hh, enable_atlas_plugin=bool(atlas_index_path))
    lf = os.path.join(wdir, "link.txt")
    Path(lf).write_text(link, encoding="utf-8")
    os.chmod(lf, 0o600)

    env = {**os.environ, "HERMES_HOME": hh, "PYTHONUNBUFFERED": "1", "PYTHONPATH": os.path.expanduser("~/.hermes/hermes-agent")}
    for k in ("PLAYBOX_HOME", "PLAYBOX_HOSTING_DIR"):
        env.pop(k, None)
    usage_path = os.path.join(wdir, "usage.json")
    stats_path = os.path.join(wdir, "delegates.jsonl")
    env.update(
        PLAYBOX_BIN=R.PLAYBOX,
        PLAYBOX_HOME=os.path.join(wdir, "mhome"),
        PLAYBOX_LINK_FILE=lf,
        PLAYBOX_IMAGE=IMG,
        PLAYBOX_STATS=stats_path,
        PLAYBOX_MCP_LOG=os.path.join(wdir, "mcp.log"),
        PLAYBOX_ALLOW_MISSING_LIMITS="1",
        PLAYBOX_TRUST_OFFLINE_WORKSPACE="1",
        PLAYBOX_KEEP="1" if keep else "0",
        TERMINAL_ENV="playbox",
        TERMINAL_CWD="/sandbox",
    )
    if approvers_hex:
        env["PLAYBOX_APPROVERS"] = approvers_hex
    if atlas_index_path:
        env["PLAYBOX_ATLAS_INDEX"] = atlas_index_path

    py = "/usr/local/venv/bin/python"
    prefix = (atlas_brief + "\n\n") if atlas_brief else ""
    prompt = (
        f"{prefix}Write a minimal, fast standalone Python reproduction script at `/sandbox/_pbx_repro.py` "
        f"(do NOT fix the bug or edit any existing repository files). "
        f"Rules for `/sandbox/_pbx_repro.py`:\n"
        f"1. Call the lowest-level library function/method described in the issue directly with realistic inputs "
        f"(or spawn a real child process if testing `__main__` / CLI startup — NEVER mutate `sys.argv` in-process "
        f"without setting `sys.modules['__main__'].__file__`, because Python sets `__main__.__file__` at process start).\n"
        f"2. If Django settings are needed, configure minimal in-memory settings at the top (`from django.conf import settings; "
        f"settings.configure(...)` and `django.setup()`).\n"
        f"3. Assert the expected fixed behavior with `assert ...` so `/sandbox/_pbx_repro.py` FAILS with `AssertionError` "
        f"on the current buggy code and exits 0 once the bug is fixed.\n"
        f"4. Run `PYTHONPATH=/sandbox {py} /sandbox/_pbx_repro.py` and verify it fails with `AssertionError` "
        f"(any `SyntaxError`, `ImportError`, `ModuleNotFoundError`, or `NameError` will be automatically rejected), "
        f"then stop and reply with one line.\n\nISSUE:\n{r['problem_statement']}"
    )
    R.kill_my_gateways(hh)
    cmd = [*R.HERMES, "-z", prompt, "-m", model, "--provider", provider, "-t", "terminal,file", "--usage-file", usage_path, "--yolo"]
    try:
        subprocess.run(cmd, cwd=wdir, env=env, capture_output=True, text=True, timeout=min(timeout, 300))
    except subprocess.TimeoutExpired:
        pass
    R.kill_my_gateways(hh)

    try:
        u = json.loads(Path(usage_path).read_text(encoding="utf-8"))
    except Exception:  # noqa: BLE001
        u = {}
    fin_path = Path(wdir, "delegates.final.json")
    fin = json.loads(fin_path.read_text(encoding="utf-8")) if fin_path.exists() else {}
    patch = fin.get("patch") or ""
    # Extract `/sandbox/_pbx_repro.py` content cleanly from the new-file git diff.
    script_lines: list[str] = []
    in_repro = False
    for line in patch.splitlines():
        if line.startswith("diff --git "):
            in_repro = "b/_pbx_repro.py" in line
            continue
        if not in_repro:
            continue
        if line.startswith("+++") or line.startswith("---") or line.startswith("@@") or line.startswith("new file") or line.startswith("index "):
            continue
        if line.startswith("+"):
            script_lines.append(line[1:])
    script = "\n".join(script_lines).strip()
    if script:
        fails_on_base, sanity_ok, reject_reason, base_out = _check_repro_fails_on_base(r, script)
    else:
        fails_on_base, sanity_ok, reject_reason, base_out = (
            False,
            False,
            "no _pbx_repro.py created",
            "no _pbx_repro.py created",
        )
    return ReproGateResult(
        script=script,
        fails_on_base=fails_on_base,
        cost_usd=estimate_usage_cost_usd(u, model),
        base_output=base_out,
        sanity_ok=sanity_ok,
        reject_reason=reject_reason,
    )


def run_worker(
    worker_label: str,
    worker_idx: int,
    *,
    inst: str,
    r: dict,
    run_dir: str,
    host,
    link: str,
    provider: str,
    model: str,
    atlas_brief: str,
    atlas_index_path: str,
    timeout: int,
    keep: bool,
    extra_guidance: str = "",
    approvers_hex: str = "",
    allow_egress: list[str] | None = None,
) -> CandidateResult:
    wdir = os.path.join(run_dir, worker_label)
    os.makedirs(wdir, exist_ok=True)
    hh = os.path.join(R.D, f"home-P2-{worker_label}")
    _prepare_worker_home(hh, enable_atlas_plugin=bool(atlas_index_path))
    lf = os.path.join(wdir, "link.txt")
    Path(lf).write_text(link, encoding="utf-8")
    os.chmod(lf, 0o600)

    env = {**os.environ, "HERMES_HOME": hh, "PYTHONUNBUFFERED": "1", "PYTHONPATH": os.path.expanduser("~/.hermes/hermes-agent")}
    for k in ("PLAYBOX_HOME", "PLAYBOX_HOSTING_DIR"):
        env.pop(k, None)
    usage_path = os.path.join(wdir, "usage.json")
    stats_path = os.path.join(wdir, "delegates.jsonl")
    env.update(
        PLAYBOX_BIN=R.PLAYBOX,
        PLAYBOX_HOME=os.path.join(wdir, "mhome"),
        PLAYBOX_LINK_FILE=lf,
        PLAYBOX_IMAGE=IMG,
        PLAYBOX_STATS=stats_path,
        PLAYBOX_MCP_LOG=os.path.join(wdir, "mcp.log"),
        PLAYBOX_ALLOW_MISSING_LIMITS="1",
        PLAYBOX_TRUST_OFFLINE_WORKSPACE="1",
        PLAYBOX_KEEP="1" if keep else "0",
        TERMINAL_ENV="playbox",
        TERMINAL_CWD="/sandbox",
    )
    if approvers_hex:
        env["PLAYBOX_APPROVERS"] = approvers_hex
    if allow_egress:
        env["PLAYBOX_EGRESS"] = ",".join(allow_egress)
    if atlas_index_path:
        env["PLAYBOX_ATLAS_INDEX"] = atlas_index_path

    repo, py = "/sandbox", "/usr/local/venv/bin/python"
    mod = re.search(r"\((.+)\)", r["F"][0]).group(1).rsplit(".", 1)[0]
    hint = f"cd tests && PYTHONPATH={repo} {py} runtests.py --settings=test_sqlite --parallel 1 {mod}"
    prefix = (atlas_brief + "\n\n") if atlas_brief else ""
    if extra_guidance:
        diversity = extra_guidance.strip() + " "
    elif worker_idx == 0:
        diversity = (
            "Follow the call chain down to the lowest-level core function responsible for the bug rather than "
            "patching a high-level wrapper, so direct unit tests and end-to-end flows both succeed. "
        )
    else:
        diversity = f"Focus on root-cause elegance and edge cases (independent attempt #{worker_idx + 1}). "

    prompt = (
        f"{prefix}The repository in {repo} has a bug. {diversity}Fix it in the source code so the behaviour described in the issue is correct, "
        f"without breaking existing behaviour. Use the Python at {py} (all dependencies are installed). "
        f"Example test command from the repo root: `{hint}`. "
        f"Do not edit existing tests; hidden tests will check your fix. Edit files in place, then reply with a one-line summary.\n\nISSUE:\n{r['problem_statement']}"
    )
    R.kill_my_gateways(hh)
    cmd = [*R.HERMES, "-z", prompt, "-m", model, "--provider", provider, "-t", "terminal,file", "--usage-file", usage_path, "--yolo"]
    t0 = time.time()
    try:
        p = subprocess.run(cmd, cwd=wdir, env=env, capture_output=True, text=True, timeout=timeout)
        stdout, stderr = p.stdout, p.stderr
    except subprocess.TimeoutExpired as e:
        stdout = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
        stderr = "timeout"
    wall = time.time() - t0
    R.kill_my_gateways(hh)
    Path(wdir, "hermes.out").write_text(stdout + "\n--- stderr ---\n" + stderr, encoding="utf-8")

    try:
        u = json.loads(Path(usage_path).read_text(encoding="utf-8"))
    except Exception:  # noqa: BLE001
        u = {}
    fin_path = Path(wdir, "delegates.final.json")
    fin = json.loads(fin_path.read_text(encoding="utf-8")) if fin_path.exists() else {}
    patch = strip_tests(fin.get("patch") or "")
    dl = [json.loads(x) for x in Path(stats_path).read_text(encoding="utf-8").splitlines()] if os.path.exists(stats_path) else []
    secs = [x["secs"] for x in dl if "secs" in x]
    ok, _ = host.audit()
    return CandidateResult(
        worker_id=worker_label,
        model=model,
        patch=patch,
        cost_usd=estimate_usage_cost_usd(u, model),
        wall_s=round(wall, 1),
        audit_verified=ok,
        policy_digest=fin.get("policy_digest") or "",
        meta={
            "delegates": len(dl),
            "sync_none": sum(1 for x in dl if x.get("sync") == "none"),
            "sync_delta": sum(1 for x in dl if x.get("sync") == "delta"),
            "taint_advisories": sum(1 for x in dl if x.get("admission")),
            "median_cmd_s": R.pct(secs, 0.5),
            "usage": u,
        },
    )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("inst")
    ap.add_argument("n")
    ap.add_argument("outdir")
    ap.add_argument("--runtime", default="openshell", choices=["openshell", "libkrun", "docker"])
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--provider", default="anthropic")
    ap.add_argument("--model", default="claude-haiku-4-5-20251001")
    ap.add_argument("--escalate-model", default="", help="Optional frontier model for conditional escalation on online-gate failure")
    ap.add_argument("--escalate-provider", default="", help="Provider for escalation model (defaults to --provider)")
    ap.add_argument("--escalate-reserve", type=float, default=0.35, help="USD reserve kept for conditional escalation")
    ap.add_argument("--repro-gate", action="store_true", help="Spawn independent non-oracle repro-test worker to gate cheap candidates")
    ap.add_argument("--repro-model", default="", help="Model for --repro-gate worker (defaults to --model)")
    ap.add_argument("--repro-provider", default="", help="Provider for --repro-gate worker (defaults to --provider)")
    ap.add_argument("--egress", default="", help="Optional BOX-7 host-side egress config JSON file (keeps API keys on host)")
    ap.add_argument("--allow-egress", action="append", default=[], help="Allowed BOX-7 upstream name(s) from --egress")
    ap.add_argument("--memory-file", default="", help="Optional JSONL room memory file for AtlasKV verified lessons")
    ap.add_argument("--budget", type=float, default=0.80)
    ap.add_argument("--timeout", type=int, default=int(os.environ.get("PB_TIMEOUT", "1800")))
    ap.add_argument("--no-atlas", action="store_true")
    ap.add_argument("--legacy-replay", action="store_true")
    args = ap.parse_args()

    R.IMAGE = IMG
    r = load(args.inst)
    tag = f"{args.inst}-P2-{args.n}"
    os.makedirs(args.outdir, exist_ok=True)
    run_dir = os.path.join(R.D, f"swe-{tag}")
    shutil.rmtree(run_dir, ignore_errors=True)
    os.makedirs(run_dir)
    repo_dir = os.path.join(run_dir, "repo")
    host_repo(repo_dir, args.inst, r)

    git_isolated, commit_count = verify_single_commit_repo(repo_dir)
    if not git_isolated:
        raise RuntimeError(f"anti-leak invariant violated: base repo has {commit_count} commits (>1)")

    atlas_idx_path = "" if args.no_atlas else os.path.join(run_dir, "atlas.json")
    atlas_brief, atlas_meta = (
        ("", {"disabled": True})
        if args.no_atlas
        else build_atlas_brief(
            repo_dir,
            r["problem_statement"],
            r["base_commit"],
            index_out=atlas_idx_path,
            memory_file=args.memory_file or None,
        )
    )

    test_mod = re.search(r"\((.+)\)", r["F"][0]).group(1).rsplit(".", 1)[0]
    needed_links = args.workers + (1 if args.repro_gate else 0) + (1 if args.escalate_model else 0) + 1
    signing_key = load_or_create_signing_key(os.path.join(R.D, "attested_gate.ed25519"))
    try:
        from cryptography.hazmat.primitives import serialization
        approvers_hex = signing_key.public_key().public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        ).hex()
    except Exception:  # noqa: BLE001
        approvers_hex = ""

    host = _start_host_with_runtime(
        run_dir,
        args.runtime,
        egress_file=args.egress,
        allow_egress=args.allow_egress,
    )
    host.hub_register()
    try:
        while len(host.links) < needed_links:
            host.cmd("link")
            time.sleep(0.8)
        link_cursor = 1

        def next_link() -> str:
            nonlocal link_cursor
            lk = host.links[link_cursor]
            link_cursor += 1
            return lk

        t0 = time.time()
        repro_gate: ReproGateResult | None = None
        if args.repro_gate:
            repro_gate = run_repro_generator(
                r=r,
                run_dir=run_dir,
                host=host,
                link=next_link(),
                provider=args.repro_provider or args.provider,
                model=args.repro_model or args.model,
                atlas_brief=atlas_brief,
                atlas_index_path=atlas_idx_path,
                timeout=args.timeout,
                keep=not args.legacy_replay,
                approvers_hex=approvers_hex,
            )

        worker_links = [next_link() for _ in range(args.workers)]
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            futs = [
                pool.submit(
                    run_worker,
                    f"w{i}",
                    i,
                    inst=args.inst,
                    r=r,
                    run_dir=run_dir,
                    host=host,
                    link=worker_links[i],
                    provider=args.provider,
                    model=args.model,
                    atlas_brief=atlas_brief,
                    atlas_index_path=atlas_idx_path,
                    timeout=args.timeout,
                    keep=not args.legacy_replay,
                    approvers_hex=approvers_hex,
                    allow_egress=args.allow_egress,
                )
                for i in range(args.workers)
            ]
            cheap_candidates = [f.result() for f in futs]

        # Non-oracle online gate: existing tests + valid non-vacuous repro script (NEVER hidden SWE tests).
        repro_script = repro_gate.script if (repro_gate and repro_gate.valid) else ""

        def online_verify(patch: str):
            return _run_sealed_container_check(r, patch, test_mod=test_mod, repro_script=repro_script)

        all_workers_meta = [c.meta for c in cheap_candidates]
        escalate_fn = None
        if args.escalate_model:
            esc_link = next_link()
            esc_prov = args.escalate_provider or args.provider

            def _escalate(brief: str, _remaining_usd: float) -> CandidateResult:
                cand = run_worker(
                    "w_escalate",
                    99,
                    inst=args.inst,
                    r=r,
                    run_dir=run_dir,
                    host=host,
                    link=esc_link,
                    provider=esc_prov,
                    model=args.escalate_model,
                    atlas_brief=brief,
                    atlas_index_path=atlas_idx_path,
                    timeout=args.timeout,
                    keep=not args.legacy_replay,
                    extra_guidance="Prior cheap attempts failed the independent check. Trace the core call chain carefully.",
                    approvers_hex=approvers_hex,
                    allow_egress=args.allow_egress,
                )
                all_workers_meta.append(cand.meta)
                return cand

            escalate_fn = _escalate

        budget = BudgetCap(
            max_total_usd=args.budget,
            max_worker_usd=args.budget / max(args.workers, 1),
            escalate_reserve_usd=args.escalate_reserve if args.escalate_model else 0.0,
        )
        decision = run_cascade(
            repo_dir,
            cheap_candidates,
            online_verify=online_verify,
            budget=budget,
            escalate_fn=escalate_fn,
            atlas_brief=atlas_brief,
            repro_gate=repro_gate,
        )
        wall_total = round(time.time() - t0, 1)

        winner = decision.winner
        win_patch = winner.patch if winner else ""

        # Final sealed holdout evaluation (strictly isolated from workers, repro gate, and router).
        holdout = score(args.inst, win_patch)
        holdout_resolved = bool(holdout.get("resolved"))
        host_audit_ok, _ = host.audit()

        # Emit cryptographically signed Attested Gate Receipt (`Ed25519+ML-DSA-65`).
        policy_digest = (winner.policy_digest if winner else "") or next((c.policy_digest for c in cheap_candidates if c.policy_digest), "")
        receipt = build_signed_receipt(
            instance=args.inst,
            base_commit=r["base_commit"],
            runtime=args.runtime,
            git_history_isolated=git_isolated,
            repo_commit_count=commit_count,
            policy_digest=policy_digest,
            network_isolated=not bool(args.allow_egress),
            box7_egress_hosts=list(args.allow_egress or []),
            host_audit_verified=host_audit_ok,
            audit_log_path=os.path.join(run_dir, "state", "audit.log"),
            taint_summary={
                "taint_mode": "enforced" if approvers_hex else "advisory",
                "trust_offline_workspace": True,
                "mcp_advisories": sum(int(m.get("taint_advisories") or 0) for m in all_workers_meta),
                "delegates": sum(int(m.get("delegates") or 0) for m in all_workers_meta),
                "sync_none": sum(int(m.get("sync_none") or 0) for m in all_workers_meta),
                "sync_delta": sum(int(m.get("sync_delta") or 0) for m in all_workers_meta),
            },
            atlas_summary=atlas_meta,
            router_provenance=decision.provenance,
            sealed_gate={
                "online_gate_passed": bool(winner and winner.passed),
                "holdout_resolved": holdout_resolved,
                "f2p": holdout.get("f2p"),
                "f2p_total": holdout.get("f2p_total"),
                "p2p": holdout.get("p2p"),
                "p2p_total": holdout.get("p2p_total"),
                "applied": holdout.get("applied"),
                "verifier_sandbox_isolated": True,
            },
            signing_key=signing_key,
        )
        receipt_ok, receipt_msg = verify_signed_receipt(receipt, require_pq=True)

        # Gate-verified compounding memory: persist lesson ONLY when sealed gate passed & receipt verifies.
        lesson_persisted = False
        if args.memory_file and holdout_resolved and receipt_ok and win_patch.strip():
            touched = sorted(set(re.findall(r"(?m)^diff --git a/(.+?) b/", win_patch)))
            hunks = [ln.strip() for ln in win_patch.splitlines() if ln.startswith("@@")][:3]
            added = [
                ln[1:].strip()
                for ln in win_patch.splitlines()
                if ln.startswith("+") and not ln.startswith("+++") and ln[1:].strip() and not ln[1:].strip().startswith("#")
            ][:3]
            snippet = f" [{'; '.join(added)[:140]}]" if added else ""
            append_verified_lesson(
                args.memory_file,
                topic=f"{args.inst} {' '.join(touched)}",
                lesson=f"Verified fix touched {', '.join(touched)} ({'; '.join(hunks)}){snippet}",
                evidence=[f"commit:{r['base_commit'][:12]}", f"inst:{args.inst}"],
                gate_receipt_sha256=receipt["payload_sha256"],
            )
            lesson_persisted = True

        rcpt_path = Path(args.outdir, f"{tag}.receipt.json")
        rcpt_path.write_text(json.dumps(receipt, indent=2), encoding="utf-8")

        # Also export and offline-verify a full 5-gate NIST AI RMF / EU AI Act compliance bundle (`playbox.attested_gate.v1`).
        bundle_path = Path(args.outdir, f"{tag}.gate-bundle.json")
        key_file = os.path.join(R.D, "attested_gate.ed25519")
        gate_bundle_verified = False
        gate_bundle_digest = ""
        exp_proc = subprocess.run(
            [
                R.PLAYBOX, "gate", "export",
                "--state", os.path.join(run_dir, "state"),
                "--receipt", str(rcpt_path),
                "--sign-key", key_file,
                "--out", str(bundle_path),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        if exp_proc.returncode == 0 and bundle_path.exists():
            vfy_proc = subprocess.run(
                [R.PLAYBOX, "gate", "verify", str(bundle_path), "--require-signature", "--require-pq", "--json"],
                capture_output=True,
                text=True,
                timeout=30,
            )
            if vfy_proc.returncode == 0:
                try:
                    vfy_json = json.loads(vfy_proc.stdout)
                    gate_bundle_verified = bool(vfy_json.get("ok") and vfy_json.get("pq_signed"))
                    gate_bundle_digest = str(vfy_json.get("bundle_digest") or "")
                except ValueError:
                    pass

        res = {
            "inst": args.inst,
            "arm": "P2",
            "runtime": args.runtime,
            "keep": not args.legacy_replay,
            "workers": args.workers,
            "wall_s": wall_total,
            "resolved": holdout_resolved,
            "online_gate_passed": bool(winner and winner.passed),
            "holdout": {k: v for k, v in holdout.items() if k not in ("ok", "tail")},
            "total_cost_usd": round(decision.total_cost_usd, 6),
            "composed_tried": decision.composed_tried,
            "composed_won": decision.composed_won,
            "escalated": decision.escalated,
            "escalation_won": decision.escalation_won,
            "repro_gate": decision.provenance.get("repro_gate"),
            "git_history_isolated": git_isolated,
            "receipt_verified": receipt_ok,
            "receipt_sha256": receipt["payload_sha256"],
            "gate_bundle_verified": gate_bundle_verified,
            "gate_bundle_digest": gate_bundle_digest,
            "lesson_persisted": lesson_persisted,
            "atlas": atlas_meta,
            "provenance": decision.provenance,
            "workers_meta": all_workers_meta,
        }
        Path(args.outdir, f"{tag}.json").write_text(json.dumps(res, indent=2), encoding="utf-8")
        Path(args.outdir, f"{tag}.patch").write_text(win_patch, encoding="utf-8")
        print(json.dumps({
            "inst": res["inst"],
            "arm": res["arm"],
            "runtime": res["runtime"],
            "wall_s": res["wall_s"],
            "resolved": res["resolved"],
            "online_gate_passed": res["online_gate_passed"],
            "total_cost_usd": res["total_cost_usd"],
            "escalated": res["escalated"],
            "receipt_verified": res["receipt_verified"],
            "receipt_msg": receipt_msg,
            "sync_none": receipt["payload"]["taint"]["sync_none"],
            "sync_delta": receipt["payload"]["taint"]["sync_delta"],
            "median_cmd_s": all_workers_meta[0].get("median_cmd_s") if all_workers_meta else None,
        }))
    finally:
        host.close()


if __name__ == "__main__":
    main()
