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
    AiproxyEmbedder,
    AiproxyReranker,
    HiKVPIndex,
    append_verified_lesson,
    build_repo_triples,
    format_brief,
    load_room_memory_jsonl,
)
from playbox_router import (  # noqa: E402
    AiproxyClient,
    BudgetCap,
    CandidateResult,
    ReproGateResult,
    build_signed_receipt,
    is_genuine_repro_failure,
    load_admin_key,
    load_or_create_signing_key,
    run_cascade,
    verify_signed_receipt,
    verify_single_commit_repo,
)
from score import _inst, extract_django_modules, load, score  # noqa: E402

IMG = os.environ.get("PB_SWE_IMAGE", "pb-swe-django:arm64")
SWE = os.environ.get("PB_SWE_DIR", os.path.expanduser("~/.playbox-hermes/openshell-parity/swe"))
GIT = ["git", "-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0", "-c", "user.name=bench", "-c", "user.email=bench@example.com"]


def _resolve_inst_runtime(inst: str, r: dict) -> dict:
    _, default_img = _inst(inst)
    img = os.environ.get("PB_SWE_IMAGE") or r.get("pb_image") or default_img
    parser = r.get("pb_parser") or ("pytest" if inst == "flask" else "django")
    py = r.get("pb_python") or "/usr/local/venv/bin/python"
    mods = extract_django_modules(r["F"]) if parser == "django" else []
    mod = mods[0] if mods else ""
    if r.get("pb_test_hint"):
        hint_sandbox = r["pb_test_hint"].format(repo="/sandbox", py=py)
    elif parser == "pytest":
        tfile = r["F"][0].split("::")[0] if r.get("F") else "tests"
        pp_sb = "/sandbox/src:/sandbox" if inst == "flask" else r.get("pb_pythonpath_sandbox", "/sandbox")
        hint_sandbox = f"cd /sandbox && PYTHONPATH={pp_sb} {py} -m pytest {tfile} -q"
    else:
        hint_sandbox = f"cd /sandbox/tests && PYTHONPATH=/sandbox {py} runtests.py --settings=test_sqlite --parallel 1 {mod}"

    if r.get("pb_online_cmd"):
        online_cmd = r["pb_online_cmd"]
    elif parser == "pytest":
        tfile = r["F"][0].split("::")[0] if r.get("F") else "tests"
        pp_tb = "/tmp/tb/src:/tmp/tb" if inst == "flask" else r.get("pb_pythonpath", "/tmp/tb")
        online_cmd = f"PYTHONPATH={pp_tb} {py} -m pytest {tfile} -q"
    else:
        online_cmd = f"cd tests && PYTHONPATH=/tmp/tb {py} runtests.py --settings=test_sqlite --parallel 1 {mod}"
    return {
        "img": img,
        "parser": parser,
        "py": py,
        "mod": mod,
        "hint_sandbox": hint_sandbox,
        "online_cmd": online_cmd,
    }


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
            os.makedirs(self.home, exist_ok=True)
            sec_dir = os.path.join(self.home, "secrets")
            os.makedirs(sec_dir, mode=0o700, exist_ok=True)
            os.chmod(sec_dir, 0o700)
            self.hosting = os.path.join(d_path, "hosting")
            os.makedirs(self.hosting, exist_ok=True)
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
    cache_root = os.path.join(R.D, "swe-repos")
    os.makedirs(cache_root, exist_ok=True)
    cache = os.path.join(cache_root, inst)
    if not os.path.isdir(os.path.join(cache, ".git")):
        tmp_cache = f"{cache}.tmp.{os.getpid()}"
        shutil.rmtree(tmp_cache, ignore_errors=True)
        os.makedirs(tmp_cache, exist_ok=True)
        if r.get("pb_host_tar"):
            subprocess.run(["tar", "-xf", r["pb_host_tar"], "-C", tmp_cache], check=True)
            shutil.rmtree(os.path.join(tmp_cache, ".git"), ignore_errors=True)
        else:
            fam = r.get("pb_family") or ("django" if inst.startswith("django-") else inst)
            tb = f"{SWE}/{fam}/testbed"
            subprocess.run(f"git -C {tb} archive {r['base_commit']} | tar -x -C {tmp_cache}", shell=True, check=True)
        subprocess.run([*GIT, "init", "-q", "-b", "main", tmp_cache], check=True)
        subprocess.run([*GIT, "-C", tmp_cache, "add", "-A"], check=True)
        subprocess.run([*GIT, "-C", tmp_cache, "commit", "-q", "-m", "base"], check=True)
        if not os.path.isdir(os.path.join(cache, ".git")):
            try:
                os.replace(tmp_cache, cache)
            except OSError:
                shutil.rmtree(tmp_cache, ignore_errors=True)
        else:
            shutil.rmtree(tmp_cache, ignore_errors=True)
    shutil.copytree(cache, dst, symlinks=True)


def strip_tests(patch: str) -> str:
    blocks = re.split(r"(?m)^(?=diff --git )", patch or "")
    return "".join(
        b for b in blocks
        if b.strip()
        and not re.search(r"^diff --git a/(?:.+/)?(?:tests?|testing)/", b)
        and "\nBinary files " not in b
    )


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
    if m.startswith("swe-2") and "lightning" not in m:
        return 0.0
    if "opus-5-5" in m or "opus-5.5" in m:
        in_rate, cache_rate, out_rate = 4.00, 0.20, 20.00
    elif "neon" in m:
        in_rate, cache_rate, out_rate = 3.00, 0.15, 15.00
    elif "argon" in m or "carbon" in m:
        in_rate, cache_rate, out_rate = 2.00, 0.10, 10.00
    elif any(k in m for k in ("sonnet", "terra", "opus", "gpt-5.4")):
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
    embed_fn=None,
    rerank_fn=None,
) -> tuple[str, dict]:
    t0 = time.time()
    triples = build_repo_triples(repo_dir, base_commit=base_commit)
    if memory_file:
        triples.extend(load_room_memory_jsonl(memory_file))
    idx = HiKVPIndex.build(triples, embed_fn=embed_fn, rerank_fn=rerank_fn)
    if index_out:
        idx.save(index_out)
    hits = idx.query(issue_text, top_root=5, top_inter=12, top_leaf=14, expand_hops=3, allow_untrusted=False)
    brief = format_brief(hits, max_chars=3200)
    return brief, {
        "build_and_query_s": round(time.time() - t0, 3),
        "hits": len(hits),
        "hops": 3,
        "hybrid_dense": embed_fn is not None,
        "reranked": rerank_fn is not None,
        **idx.stats(),
    }


def _prepare_worker_home(hh: str, *, enable_atlas_plugin: bool, use_aiproxy: bool = False) -> None:
    R.make_home(hh, "P", str(REPO_ROOT / "hermes-playbox-backend" / "playbox_terminal"))
    real_auth = os.path.expanduser("~/.hermes/auth.json")
    hh_auth = os.path.join(hh, "auth.json")
    if use_aiproxy:
        # Never inherit personal OAuth pool or custom provider blocks when routing through playbox-aiproxy;
        # workers use short-lived, hard-capped gateway tokens via ANTHROPIC_BASE_URL / CUSTOM_BASE_URL.
        if os.path.lexists(hh_auth):
            try:
                os.remove(hh_auth)
            except OSError:
                pass
        subprocess.run(
            [
                R.HVENV_PY, "-c",
                "import yaml,sys; p=sys.argv[1]; c=yaml.safe_load(open(p)); "
                "c.pop('providers',None); c.pop('custom_providers',None); "
                "open(p,'w').write(yaml.safe_dump(c))",
                os.path.join(hh, "config.yaml"),
            ],
            check=True,
        )
    elif os.path.exists(real_auth) and not os.path.islink(hh_auth):
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


DEVIN_BIN = os.environ.get(
    "DEVIN_BIN",
    os.path.expanduser("~/.local/bin/devin")
    if os.path.exists(os.path.expanduser("~/.local/bin/devin"))
    else "/Applications/Devin.app/Contents/Resources/app/extensions/windsurf/devin/bin/devin",
)
AGY_BIN = os.environ.get("AGY_BIN", os.path.expanduser("~/.local/bin/agy"))


def _parse_agy_usage(stdout: str, model: str, rc: int = 0, stderr: str = "") -> dict:
    lines = [ln.strip() for ln in (stdout or "").splitlines() if ln.strip().startswith("{")]
    if not lines:
        return {"model": model, "provider": "agy", "failed": rc != 0, "failure": (stderr or "no_json_output")[:200]}
    try:
        aj = json.loads(lines[-1])
    except ValueError:
        return {"model": model, "provider": "agy", "failed": rc != 0, "failure": "invalid_json"}
    au = aj.get("usage") or {}
    u = {
        "model": model,
        "provider": "agy",
        "session_id": str(aj.get("conversation_id") or ""),
        "api_calls": int(aj.get("num_turns") or 1),
        "input_tokens": int(au.get("input_tokens") or 0),
        "output_tokens": int(au.get("output_tokens") or 0),
        "thinking_tokens": int(au.get("thinking_tokens") or 0),
        "cache_read_tokens": int(au.get("cache_read_tokens") or 0),
        "cache_write_tokens": 0,
        "total_tokens": int(au.get("total_tokens") or 0),
        "completed": aj.get("status") == "SUCCESS",
    }
    if aj.get("status") not in ("SUCCESS", None) and rc != 0:
        u["failed"] = True
        u["failure"] = str(aj.get("error") or stderr or aj.get("status"))[:200]
    u["estimated_cost_usd"] = estimate_usage_cost_usd(u, model)
    return u


class _DevinPlayboxBridge:
    """In-process Playbox room session server exposing a Unix socket (`pbx.sock`) for a `devin` CLI worker.

    Why this architecture:
    - A Playbox join link is single-use. By holding `_Mcp` + stateful `keep=True` inside this bridge thread,
      every `pbx "<cmd>"` shell invocation from `devin` reuses the same authenticated Playbox member and
      persistent OpenShell `/sandbox` microVM at ~0.07 s (`sync="none"`) / ~0.4 s (`sync="delta"`).
    - `devin` runs inside an isolated empty workspace `wdir/devin_ws` where the repository does NOT exist on
      the host path; the ONLY way to inspect or modify `/sandbox` is through the governed `pbx` bridge.
    """

    _ENV_LOCK = __import__("threading").Lock()

    def __init__(self, *, wdir: str, env_vars: dict[str, str]):
        import socket
        import threading
        import types

        self.wdir = wdir
        import hashlib, tempfile
        sock_hash = hashlib.sha256(wdir.encode("utf-8")).hexdigest()[:16]
        self.sock_path = os.path.join(tempfile.gettempdir(), f"pbx-{sock_hash}.sock")
        if os.path.exists(self.sock_path):
            os.remove(self.sock_path)

        # Ensure Hermes base stubs exist if importing playbox_terminal outside Hermes venv.
        with _DevinPlayboxBridge._ENV_LOCK:
            if "agent.terminal_env_provider" not in sys.modules:
                agent_mod = types.ModuleType("agent")
                tep_mod = types.ModuleType("agent.terminal_env_provider")
                tep_mod.TerminalEnvironmentProvider = type("TerminalEnvironmentProvider", (), {})
                tools_mod = types.ModuleType("tools")
                envs_mod = types.ModuleType("tools.environments")
                base_mod = types.ModuleType("tools.environments.base")

                class _BaseEnv:
                    def __init__(self, cwd="/sandbox", timeout=120):
                        self.cwd = cwd
                        self.timeout = timeout

                base_mod.BaseEnvironment = _BaseEnv
                sys.modules["agent"] = agent_mod
                sys.modules["agent.terminal_env_provider"] = tep_mod
                sys.modules["tools"] = tools_mod
                sys.modules["tools.environments"] = envs_mod
                sys.modules["tools.environments.base"] = base_mod

            import playbox_terminal as pt

            old_env = os.environ.copy()
            try:
                os.environ.update(env_vars)
                # Create a fresh isolated _Session instance for this worker (do not share class singleton across workers).
                sess = pt._Session()
                self.pbx_env = pt.PlayboxEnvironment.__new__(pt.PlayboxEnvironment)
                self.pbx_env.s = sess
                self.pbx_env.cwd = "/sandbox"
                self.pbx_env.timeout = 180
            finally:
                os.environ.clear()
                os.environ.update(old_env)

        self._srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._srv.bind(self.sock_path)
        os.chmod(self.sock_path, 0o600)
        self._srv.listen(16)
        self._stop = False
        self._thr = threading.Thread(target=self._serve, daemon=True)
        self._thr.start()

    def _serve(self) -> None:
        while not self._stop:
            try:
                conn, _ = self._srv.accept()
            except OSError:
                break
            try:
                raw = b""
                while True:
                    chunk = conn.recv(65536)
                    if not chunk:
                        break
                    raw += chunk
                    if b"\n" in raw:
                        break
                req = json.loads(raw.decode("utf-8", "ignore").strip() or "{}")
                cmd = str(req.get("command") or "")
                cwd = str(req.get("cwd") or "/sandbox")
                stdin_data = req.get("stdin_data")
                timeout = req.get("timeout")
                res = self.pbx_env.execute(cmd, cwd=cwd, timeout=timeout, stdin_data=stdin_data)
                resp = json.dumps({"output": res.get("output", ""), "returncode": int(res.get("returncode", 0))}) + "\n"
                conn.sendall(resp.encode("utf-8"))
            except Exception as exc:  # noqa: BLE001
                try:
                    conn.sendall((json.dumps({"output": f"[pbx bridge error: {exc}]\n", "returncode": 1}) + "\n").encode("utf-8"))
                except OSError:
                    pass
            finally:
                try:
                    conn.close()
                except OSError:
                    pass

    def close_and_export(self) -> str:
        self._stop = True
        try:
            self._srv.close()
        except OSError:
            pass
        try:
            patch = self.pbx_env.export_patch()
        finally:
            self.pbx_env.s.mcp.close()
            if os.path.exists(self.sock_path):
                try:
                    os.remove(self.sock_path)
                except OSError:
                    pass
        return patch


def _write_devin_wrappers(wdir: str, sock_path: str, atlas_index_path: str) -> str:
    """Create `pbx` and `atlas_query` CLI shims in `wdir/bin` for the Devin worker."""
    bin_dir = os.path.join(wdir, "bin")
    os.makedirs(bin_dir, exist_ok=True)
    pbx_script = os.path.join(bin_dir, "pbx")
    Path(pbx_script).write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, socket, sys\n"
        f"SOCK = {sock_path!r}\n"
        "args = sys.argv[1:]\n"
        "stdin_data = None\n"
        "if args and args[0] == '--stdin':\n"
        "    args = args[1:]\n"
        "    stdin_data = sys.stdin.read()\n"
        "elif args and args[0] == '--write' and len(args) >= 2:\n"
        "    target = args[1]\n"
        "    stdin_data = sys.stdin.read()\n"
        "    import shlex\n"
        "    q = shlex.quote(target)\n"
        "    args = [f'tmp=\"{target}.pbx.$$\" && cat > \"$tmp\" && mv -f \"$tmp\" {q}']\n"
        "cmd = ' '.join(args) if args else sys.stdin.read()\n"
        "s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n"
        "s.connect(SOCK)\n"
        "s.sendall((json.dumps({'command': cmd, 'cwd': '/sandbox', 'stdin_data': stdin_data}) + '\\n').encode('utf-8'))\n"
        "buf = b''\n"
        "while True:\n"
        "    c = s.recv(65536)\n"
        "    if not c:\n"
        "        break\n"
        "    buf += c\n"
        "s.close()\n"
        "r = json.loads(buf.decode('utf-8', 'ignore'))\n"
        "sys.stdout.write(r.get('output', ''))\n"
        "sys.exit(int(r.get('returncode', 0)))\n",
        encoding="utf-8",
    )
    os.chmod(pbx_script, 0o755)

    aq_script = os.path.join(bin_dir, "atlas_query")
    Path(aq_script).write_text(
        "#!/usr/bin/env python3\n"
        "import os, sys\n"
        f"sys.path.insert(0, {str(REPO_ROOT / 'hermes-playbox-backend')!r})\n"
        f"os.environ['PLAYBOX_ATLAS_INDEX'] = {atlas_index_path!r}\n"
        "from playbox_atlas import query_atlas\n"
        "q = ' '.join(sys.argv[1:]).strip()\n"
        "print(query_atlas(q, top_k=14, expand_hops=3))\n",
        encoding="utf-8",
    )
    os.chmod(aq_script, 0o755)
    return bin_dir


def _extract_devin_usage(devin_ws: str, t_start: float, model: str) -> dict:
    """Query `~/.local/share/devin/cli/sessions.db` for the exact token & latency telemetry of this Devin run."""
    import sqlite3

    db_path = Path.home() / ".local/share/devin/cli/sessions.db"
    if not db_path.exists():
        return {"model": model, "provider": "devin"}
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        ws_real = str(Path(devin_ws).resolve())
        rows = conn.execute(
            "SELECT id, model, created_at FROM sessions WHERE created_at >= ? ORDER BY created_at DESC LIMIT 100",
            (int(t_start) - 5,),
        ).fetchall()
        sid = None
        for cand_id, cand_model, _ in rows:
            wdir_row = conn.execute("SELECT working_directory FROM sessions WHERE id=?", (cand_id,)).fetchone()
            wdir_str = str((wdir_row or [""])[0])
            if devin_ws in wdir_str or ws_real in wdir_str:
                sid = cand_id
                break
        if sid is None and len(rows) == 1:
            sid = rows[0][0]
        if sid is None:
            return {"model": model, "provider": "devin"}

        seen_reqs: set[str] = set()
        inp_total = 0
        out_total = 0
        cache_read_total = 0
        cache_write_total = 0
        api_calls = 0
        gen_model = model
        for (cm_raw,) in conn.execute("SELECT chat_message FROM message_nodes WHERE session_id=? ORDER BY row_id", (sid,)).fetchall():
            try:
                cm = json.loads(cm_raw)
            except ValueError:
                continue
            if cm.get("role") != "assistant":
                continue
            meta = cm.get("metadata") or {}
            req_id = str(meta.get("request_id") or "")
            if not req_id or req_id in seen_reqs:
                continue
            seen_reqs.add(req_id)
            metrics = meta.get("metrics") or {}
            if not metrics:
                continue
            api_calls += 1
            gen_model = str(meta.get("generation_model") or gen_model)
            # In Devin metrics: `input_tokens` is uncached+cache_read, `cache_creation_tokens` is cache write.
            uncached_or_in = int(metrics.get("input_tokens") or 0)
            c_read = int(metrics.get("cache_read_tokens") or 0)
            c_write = int(metrics.get("cache_creation_tokens") or 0)
            out_tok = int(metrics.get("output_tokens") or 0)
            inp_total += uncached_or_in + c_write
            cache_read_total += c_read
            cache_write_total += c_write
            out_total += out_tok

        usage = {
            "model": gen_model,
            "provider": "devin",
            "session_id": sid,
            "api_calls": api_calls,
            "input_tokens": inp_total,
            "output_tokens": out_total,
            "cache_read_tokens": cache_read_total,
            "cache_write_tokens": cache_write_total,
            "total_tokens": inp_total + out_total,
            "completed": True,
        }
        usage["estimated_cost_usd"] = estimate_usage_cost_usd(usage, gen_model)
        return usage
    except Exception as exc:  # noqa: BLE001
        return {"model": model, "provider": "devin", "usage_error": str(exc)}


def _run_sealed_container_check(
    r: dict,
    patch: str,
    *,
    rt_info: dict,
    repro_script: str = "",
) -> tuple[bool, float, dict]:
    """Non-oracle online gate inside a sealed no-network container.

    Runs existing repository tests (`online_cmd`, weight 0.6) and optional independent reproduction
    script (`repro_script`, weight 0.4) separately so passing the authoritative repo test suite
    is scored distinctly from a synthetic repro failure. Never touches hidden SWE-bench `test_patch`.
    """
    img = rt_info["img"]
    online_cmd = rt_info["online_cmd"]
    py = rt_info["py"]
    with tempfile.TemporaryDirectory(prefix="pbx-online-gate-") as td:
        Path(td, "agent.patch").write_text(patch or "", encoding="utf-8")
        if repro_script.strip():
            Path(td, "repro.py").write_text(repro_script, encoding="utf-8")
        if r.get("pb_host_tar"):
            shutil.copy(r["pb_host_tar"], os.path.join(td, "repo.tar"))
            prep = "mkdir -p /tmp/tb && tar -xf /s/repo.tar -C /tmp/tb && cd /tmp/tb"
        else:
            prep = f"mkdir -p /tmp/tb && git -C /usr/local/testbed archive {r['base_commit']} | tar -x -C /tmp/tb && cd /tmp/tb"
        apply_step = "git apply /s/agent.patch && " if (patch or "").strip() else ""
        has_repro = bool(repro_script.strip())
        if has_repro:
            sh = (
                f"{prep} && {apply_step}"
                f"({online_cmd}); "
                f"__EX=$?; echo ___PBX_EXISTING_RC=$__EX; "
                f"PYTHONPATH=/tmp/tb/src:/tmp/tb {py} /s/repro.py; "
                f"__RP=$?; echo ___PBX_REPRO_RC=$__RP"
            )
        else:
            sh = (
                f"{prep} && {apply_step}"
                f"({online_cmd}); "
                f"__EX=$?; echo ___PBX_EXISTING_RC=$__EX"
            )
        p = subprocess.run(
            ["docker", "run", "--rm", "--network", "none", "-v", f"{td}:/s:ro", "--entrypoint", "bash", img, "-c", sh],
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


def _check_repro_fails_on_base(r: dict, repro_script: str, *, rt_info: dict) -> tuple[bool, bool, str, str]:
    """Anti-vacuous + sanity check: independent repro script MUST exit non-zero on unpatched `base_commit`
    due to a genuine behavioral/assertion failure (never `SyntaxError`, `ImportError`, `ModuleNotFoundError`, `NameError`)."""
    if not repro_script.strip():
        return False, False, "empty repro script", "empty repro script"
    ok_ast, ast_reason = is_genuine_repro_failure("", repro_script)
    if not ok_ast:
        return False, False, ast_reason, ast_reason
    img = rt_info["img"]
    py = rt_info["py"]
    with tempfile.TemporaryDirectory(prefix="pbx-repro-base-") as td:
        Path(td, "repro.py").write_text(repro_script, encoding="utf-8")
        if r.get("pb_host_tar"):
            shutil.copy(r["pb_host_tar"], os.path.join(td, "repo.tar"))
            prep = "mkdir -p /tmp/tb && tar -xf /s/repo.tar -C /tmp/tb && cd /tmp/tb"
        else:
            prep = f"mkdir -p /tmp/tb && git -C /usr/local/testbed archive {r['base_commit']} | tar -x -C /tmp/tb && cd /tmp/tb"
        sh = f"{prep} && PYTHONPATH=/tmp/tb/src:/tmp/tb {py} /s/repro.py"
        p = subprocess.run(
            ["docker", "run", "--rm", "--network", "none", "-v", f"{td}:/s:ro", "--entrypoint", "bash", img, "-c", sh],
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
    rt_info: dict,
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
    harness: str = "hermes",
    worker_slot: str = "0",
    aiproxy_client: AiproxyClient | None = None,
    aiproxy_group: str = "bench:repro",
    aiproxy_hard_usd_cap: float = 0.15,
    aiproxy_run_id: str = "",
    aiproxy_failover_policy: str = "same_model_only",
    aiproxy_cache_mode: str = "off",
) -> ReproGateResult:
    """Run an independent worker that sees ONLY the issue + AtlasKV (never any solver patch)
    and writes a standalone reproduction script `/sandbox/_pbx_repro.py` that fails on base_commit."""
    wdir = os.path.join(run_dir, "w_repro")
    os.makedirs(wdir, exist_ok=True)
    lf = os.path.join(wdir, "link.txt")
    Path(lf).write_text(link, encoding="utf-8")
    os.chmod(lf, 0o600)
    usage_path = os.path.join(wdir, "usage.json")
    stats_path = os.path.join(wdir, "delegates.jsonl")

    pbx_env_vars = {
        "PLAYBOX_BIN": R.PLAYBOX,
        "PLAYBOX_HOME": os.path.join(wdir, "mhome"),
        "PLAYBOX_LINK_FILE": lf,
        "PLAYBOX_IMAGE": rt_info["img"],
        "PLAYBOX_STATS": stats_path,
        "PLAYBOX_MCP_LOG": os.path.join(wdir, "mcp.log"),
        "PLAYBOX_ALLOW_MISSING_LIMITS": "1",
        "PLAYBOX_TRUST_OFFLINE_WORKSPACE": "1",
        "PLAYBOX_KEEP": "1" if keep else "0",
        "TERMINAL_ENV": "playbox",
        "TERMINAL_CWD": "/sandbox",
    }
    if approvers_hex:
        pbx_env_vars["PLAYBOX_APPROVERS"] = approvers_hex
    if atlas_index_path:
        pbx_env_vars["PLAYBOX_ATLAS_INDEX"] = atlas_index_path

    py = rt_info["py"]
    prefix = (atlas_brief + "\n\n") if atlas_brief else ""
    rules_body = (
        f"Rules for `/sandbox/_pbx_repro.py`:\n"
        f"1. Call the lowest-level library function/method described in the issue directly with realistic inputs "
        f"(or spawn a real child process if testing `__main__` / CLI startup — NEVER mutate `sys.argv` in-process "
        f"without setting `sys.modules['__main__'].__file__`, because Python sets `__main__.__file__` at process start).\n"
        f"2. If Django settings are needed, configure minimal in-memory settings at the top (`from django.conf import settings; "
        f"settings.configure(...)` and `django.setup()`).\n"
        f"3. Assert the expected fixed behavior with `assert ...` covering BOTH the literal example in the issue "
        f"AND edge cases implied by the general phrasing in the issue (e.g., if the issue says 'other types of iterables' "
        f"or general containers, test both `list` and `tuple`) so partial fixes fail `/sandbox/_pbx_repro.py` with "
        f"`AssertionError` and only complete fixes exit 0.\n"
    )
    import contextlib

    lease_cm = (
        aiproxy_client.worker_lease(
            group=aiproxy_group,
            worker_label="w_repro",
            hard_usd_cap=aiproxy_hard_usd_cap,
            run_id=aiproxy_run_id,
            failover_policy=aiproxy_failover_policy,
            cache_mode=aiproxy_cache_mode,
        )
        if aiproxy_client is not None
        else contextlib.nullcontext(None)
    )
    with lease_cm as lease:
        gw_env = lease.env_overrides() if lease is not None else {}
        if harness in ("devin", "agy"):
            devin_ws = os.path.join(wdir, "devin_ws")
            os.makedirs(os.path.join(devin_ws, ".devin"), exist_ok=True)
            Path(devin_ws, ".devin", "mcp_config.local.json").write_text(
                json.dumps({"mcpServers": {"playbox": {"disabled": True}}}),
                encoding="utf-8",
            )
            bridge = _DevinPlayboxBridge(wdir=wdir, env_vars=pbx_env_vars)
            bin_dir = _write_devin_wrappers(wdir, bridge.sock_path, atlas_index_path)
            prompt = (
                f"{prefix}The repository is inside a governed Playbox OpenShell microVM at `/sandbox`. "
                f"Write a minimal, fast standalone Python reproduction script at `/sandbox/_pbx_repro.py` using "
                f"`pbx --write /sandbox/_pbx_repro.py <<'EOF' ... EOF` (do NOT fix the bug or edit any existing repository files).\n"
                f"{rules_body}"
                f"4. Run `pbx \"PYTHONPATH=/sandbox/src:/sandbox {py} /sandbox/_pbx_repro.py\"` and verify it fails with `AssertionError` "
                f"(any `SyntaxError`, `ImportError`, `ModuleNotFoundError`, or `NameError` will be automatically rejected), "
                f"then stop and reply with one line.\n\nISSUE:\n{r['problem_statement']}"
            )
            devin_env = {**os.environ, **gw_env, "PATH": f"{bin_dir}:{os.environ.get('PATH', '')}", "PLAYBOX_ATLAS_INDEX": atlas_index_path}
            for _k in ("ACP_BACKEND", "DEVIN_FD_PATH", "AI_AGENT"):
                devin_env.pop(_k, None)
            if harness == "agy":
                cmd = [
                    AGY_BIN, "--model", model, "--output-format", "json",
                    "--dangerously-skip-permissions", "--disable-slash-commands", "-p", prompt,
                ]
            else:
                cmd = [
                    DEVIN_BIN, "--model", model, "--permission-mode", "dangerous",
                    "--respect-workspace-trust", "false", "--process-model", "in-process", "--verbosity", "1", "-p", prompt,
                ]
            t0 = time.time()
            rc_rep, out_rep, err_rep = 0, "", ""
            try:
                p_rep = subprocess.run(cmd, cwd=devin_ws, env=devin_env, capture_output=True, text=True, timeout=min(timeout, 300))
                rc_rep, out_rep, err_rep = p_rep.returncode, p_rep.stdout, p_rep.stderr
            except subprocess.TimeoutExpired as exc:
                rc_rep = 124
                Path(wdir, "repro.timeout").write_text(str(exc), encoding="utf-8")
            bridge.close_and_export()
            u = _parse_agy_usage(out_rep, model, rc_rep, err_rep) if harness == "agy" else _extract_devin_usage(devin_ws, t0, model)
            Path(usage_path).write_text(json.dumps(u, indent=2), encoding="utf-8")
        else:
            hh = os.path.join(R.D, f"home-P2-s{worker_slot}-repro")
            _prepare_worker_home(hh, enable_atlas_plugin=bool(atlas_index_path), use_aiproxy=lease is not None)
            env = {**os.environ, **gw_env, "HERMES_HOME": hh, "PYTHONUNBUFFERED": "1", "PYTHONPATH": os.path.expanduser("~/.hermes/hermes-agent")}
            for k in ("PLAYBOX_HOME", "PLAYBOX_HOSTING_DIR"):
                env.pop(k, None)
            env.update(pbx_env_vars)
            prompt = (
                f"{prefix}Write a minimal, fast standalone Python reproduction script at `/sandbox/_pbx_repro.py` "
                f"(do NOT fix the bug or edit any existing repository files). "
                f"{rules_body}"
                f"4. Run `PYTHONPATH=/sandbox {py} /sandbox/_pbx_repro.py` and verify it fails with `AssertionError` "
                f"(any `SyntaxError`, `ImportError`, `ModuleNotFoundError`, or `NameError` will be automatically rejected), "
                f"then stop and reply with one line.\n\nISSUE:\n{r['problem_statement']}"
            )
            R.kill_my_gateways(hh)
            cmd = [*R.HERMES, "-z", prompt, "-m", model, "--provider", provider, "-t", "terminal,file", "--usage-file", usage_path, "--yolo"]
            try:
                subprocess.run(cmd, cwd=wdir, env=env, capture_output=True, text=True, timeout=min(timeout, 300))
            except subprocess.TimeoutExpired as exc:
                Path(wdir, "repro.timeout").write_text(str(exc), encoding="utf-8")
            R.kill_my_gateways(hh)
            try:
                u = json.loads(Path(usage_path).read_text(encoding="utf-8"))
            except Exception:  # noqa: BLE001
                u = {}
    repro_cost = (
        lease.host_cost_usd
        if (lease is not None and lease.host_cost_usd is not None)
        else estimate_usage_cost_usd(u, model)
    )
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
        fails_on_base, sanity_ok, reject_reason, base_out = _check_repro_fails_on_base(r, script, rt_info=rt_info)
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
        cost_usd=repro_cost,
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
    rt_info: dict,
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
    harness: str = "hermes",
    worker_slot: str = "0",
    aiproxy_client: AiproxyClient | None = None,
    aiproxy_group: str = "bench:solver",
    aiproxy_hard_usd_cap: float = 0.25,
    aiproxy_run_id: str = "",
    aiproxy_failover_policy: str = "same_model_only",
    aiproxy_cache_mode: str = "off",
) -> CandidateResult:
    wdir = os.path.join(run_dir, worker_label)
    os.makedirs(wdir, exist_ok=True)
    lf = os.path.join(wdir, "link.txt")
    Path(lf).write_text(link, encoding="utf-8")
    os.chmod(lf, 0o600)
    usage_path = os.path.join(wdir, "usage.json")
    stats_path = os.path.join(wdir, "delegates.jsonl")

    pbx_env_vars = {
        "PLAYBOX_BIN": R.PLAYBOX,
        "PLAYBOX_HOME": os.path.join(wdir, "mhome"),
        "PLAYBOX_LINK_FILE": lf,
        "PLAYBOX_IMAGE": rt_info["img"],
        "PLAYBOX_STATS": stats_path,
        "PLAYBOX_MCP_LOG": os.path.join(wdir, "mcp.log"),
        "PLAYBOX_ALLOW_MISSING_LIMITS": "1",
        "PLAYBOX_TRUST_OFFLINE_WORKSPACE": "1",
        "PLAYBOX_KEEP": "1" if keep else "0",
        "TERMINAL_ENV": "playbox",
        "TERMINAL_CWD": "/sandbox",
    }
    if approvers_hex:
        pbx_env_vars["PLAYBOX_APPROVERS"] = approvers_hex
    if allow_egress:
        pbx_env_vars["PLAYBOX_EGRESS"] = ",".join(allow_egress)
    if atlas_index_path:
        pbx_env_vars["PLAYBOX_ATLAS_INDEX"] = atlas_index_path

    repo, py = "/sandbox", rt_info["py"]
    hint = rt_info["hint_sandbox"]
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

    import contextlib

    lease_cm = (
        aiproxy_client.worker_lease(
            group=aiproxy_group,
            worker_label=worker_label,
            hard_usd_cap=aiproxy_hard_usd_cap,
            run_id=aiproxy_run_id,
            failover_policy=aiproxy_failover_policy,
            cache_mode=aiproxy_cache_mode,
        )
        if aiproxy_client is not None
        else contextlib.nullcontext(None)
    )
    with lease_cm as lease:
        gw_env = lease.env_overrides() if lease is not None else {}
        if harness in ("devin", "agy"):
            devin_ws = os.path.join(wdir, "devin_ws")
            os.makedirs(os.path.join(devin_ws, ".devin"), exist_ok=True)
            # Disable global third-party MCP servers inside this isolated workspace so Devin starts immediately.
            Path(devin_ws, ".devin", "mcp_config.local.json").write_text(
                json.dumps({"mcpServers": {"playbox": {"disabled": True}}}),
                encoding="utf-8",
            )
            bridge = _DevinPlayboxBridge(wdir=wdir, env_vars=pbx_env_vars)
            bin_dir = _write_devin_wrappers(wdir, bridge.sock_path, atlas_index_path)
            prompt = (
                f"{prefix}The target repository is inside a governed Playbox OpenShell microVM at `{repo}` (it is NOT in your local directory). "
                f"Run EVERY shell command inside the microVM via `pbx \"<command>\"` (e.g. `pbx \"sed -n '100,160p' /sandbox/path.py\"`, "
                f"`pbx \"rg -n pattern /sandbox\"`), write/update files in `/sandbox` via `pbx --write /sandbox/path.py <<'EOF' ... EOF` "
                f"or inline python (`pbx \"{py} -c '...'\"`), and query the 3-hop call-graph index via `atlas_query \"<symbol or symptom>\"`. "
                f"{diversity}Fix the bug in the source code under `{repo}` so the behaviour described in the issue is correct, "
                f"without breaking existing behaviour. "
                f"Run tests via: `pbx \"{hint}\"`. "
                f"Do not leave edits in `tests/`; hidden tests will check your fix. Edit source files in place inside `/sandbox`, verify tests pass, "
                f"then reply with a one-line summary.\n\nISSUE:\n{r['problem_statement']}"
            )
            devin_env = {
                **os.environ,
                **gw_env,
                "PATH": f"{bin_dir}:{os.environ.get('PATH', '')}",
                "PLAYBOX_ATLAS_INDEX": atlas_index_path,
            }
            for _k in ("ACP_BACKEND", "DEVIN_FD_PATH", "AI_AGENT"):
                devin_env.pop(_k, None)
            if harness == "agy":
                cmd = [
                    AGY_BIN,
                    "--model", model,
                    "--output-format", "json",
                    "--dangerously-skip-permissions",
                    "--disable-slash-commands",
                    "-p", prompt,
                ]
            else:
                cmd = [
                    DEVIN_BIN,
                    "--model", model,
                    "--permission-mode", "dangerous",
                    "--respect-workspace-trust", "false",
                    "--process-model", "in-process",
                    "--verbosity", "1",
                    "-p", prompt,
                ]
            t0 = time.time()
            rc_sol = 0
            try:
                p = subprocess.run(cmd, cwd=devin_ws, env=devin_env, capture_output=True, text=True, timeout=timeout)
                rc_sol, stdout, stderr = p.returncode, p.stdout, p.stderr
            except subprocess.TimeoutExpired as e:
                rc_sol = 124
                stdout = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
                stderr = "timeout"
            wall = time.time() - t0
            bridge.close_and_export()
            Path(wdir, "hermes.out").write_text(stdout + "\n--- stderr ---\n" + stderr, encoding="utf-8")
            u = _parse_agy_usage(stdout, model, rc_sol, stderr) if harness == "agy" else _extract_devin_usage(devin_ws, t0, model)
            Path(usage_path).write_text(json.dumps(u, indent=2), encoding="utf-8")
        else:
            hh = os.path.join(R.D, f"home-P2-s{worker_slot}-{worker_label}")
            _prepare_worker_home(hh, enable_atlas_plugin=bool(atlas_index_path), use_aiproxy=lease is not None)
            env = {**os.environ, **gw_env, "HERMES_HOME": hh, "PYTHONUNBUFFERED": "1", "PYTHONPATH": os.path.expanduser("~/.hermes/hermes-agent")}
            for k in ("PLAYBOX_HOME", "PLAYBOX_HOSTING_DIR"):
                env.pop(k, None)
            env.update(pbx_env_vars)
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
    host_cost = lease.host_cost_usd if lease is not None else None
    worker_cost = host_cost if host_cost is not None else estimate_usage_cost_usd(u, model)
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
        cost_usd=worker_cost,
        wall_s=round(wall, 1),
        audit_verified=ok,
        policy_digest=fin.get("policy_digest") or "",
        meta={
            "delegates": len(dl),
            "sync_none": sum(1 for x in dl if x.get("sync") == "none"),
            "sync_delta": sum(1 for x in dl if x.get("sync") == "delta"),
            "taint_advisories": sum(1 for x in dl if x.get("admission")),
            "median_cmd_s": R.pct(secs, 0.5),
            "cost_source": "host_aiproxy" if host_cost is not None else "client_estimate",
            "usage": u,
        },
    )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("inst")
    ap.add_argument("n")
    ap.add_argument("outdir")
    ap.add_argument("--runtime", default="openshell", choices=["openshell", "libkrun", "docker"])
    ap.add_argument("--harness", default="hermes", choices=["hermes", "devin", "agy"], help="Agent CLI harness driving the Playbox room worker")
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--provider", default="anthropic")
    ap.add_argument("--model", default="claude-haiku-4-5-20251001")
    ap.add_argument("--worker-models", default="", help="Optional comma-separated model list per worker (e.g. gemini-4-neon-high,claude-opus-5-5-high)")
    ap.add_argument("--extra-guidance", default="", help="Optional recursive hill-climb guidance from prior non-oracle iteration")
    ap.add_argument("--escalate-model", default="", help="Optional frontier model for conditional escalation on online-gate failure")
    ap.add_argument("--escalate-provider", default="", help="Provider for escalation model (defaults to --provider)")
    ap.add_argument("--escalate-reserve", type=float, default=0.35, help="USD reserve kept for conditional escalation")
    ap.add_argument("--repro-gate", action="store_true", help="Spawn independent non-oracle repro-test worker to gate cheap candidates")
    ap.add_argument("--repro-model", default="", help="Model for --repro-gate worker (defaults to --model)")
    ap.add_argument("--repro-provider", default="", help="Provider for --repro-gate worker (defaults to --provider)")
    ap.add_argument("--egress", default="", help="Optional BOX-7 host-side egress config JSON file (keeps API keys on host)")
    ap.add_argument("--allow-egress", action="append", default=[], help="Allowed BOX-7 upstream name(s) from --egress")
    ap.add_argument("--aiproxy-url", default=os.environ.get("PLAYBOX_AIPROXY_URL", ""), help="Optional loopback playbox-aiproxy URL for unified failover, hard USD caps, and host-attested ledger")
    ap.add_argument("--aiproxy-admin-key-file", default=os.environ.get("PLAYBOX_AIPROXY_ADMIN_KEY_FILE", ""), help="Path to 0600 file containing playbox-aiproxy ADMIN_KEY")
    ap.add_argument("--aiproxy-group", default="bench:solver", help="Isolated playbox-aiproxy group name (default: bench:solver)")
    ap.add_argument("--aiproxy-failover-policy", default="same_model_only", choices=["same_model_only", "alias_cascade"], help="Gateway failover policy (benchmarks default to same_model_only)")
    ap.add_argument("--aiproxy-cache-mode", default="off", choices=["off", "record", "replay"], help="Gateway cache mode (official eval runs MUST use off)")
    ap.add_argument("--atlas-embed-model", default="", help="Optional playbox-aiproxy embedding model for hybrid dense playbox_atlas retrieval")
    ap.add_argument("--atlas-rerank-model", default="", help="Optional playbox-aiproxy rerank model for second-stage playbox_atlas retrieval")
    ap.add_argument("--memory-file", default="", help="Optional JSONL room memory file for AtlasKV verified lessons")
    ap.add_argument("--memory-gate", default="online", choices=["online", "holdout"], help="Gate condition for verified-memory.jsonl persistence (default: online, strictly non-oracle)")
    ap.add_argument("--worker-slot", default=os.environ.get("PB_WORKER_SLOT", "0"), help="Isolated concurrent worker slot ID (0..concurrency-1)")
    ap.add_argument("--budget", type=float, default=0.80)
    ap.add_argument("--timeout", type=int, default=int(os.environ.get("PB_TIMEOUT", "1800")))
    ap.add_argument("--no-atlas", action="store_true")
    ap.add_argument("--legacy-replay", action="store_true")
    args = ap.parse_args()

    r = load(args.inst)
    rt_info = _resolve_inst_runtime(args.inst, r)
    R.IMAGE = rt_info["img"]
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

    aiproxy_client: AiproxyClient | None = None
    atlas_embed_tok = None
    atlas_embedder = None
    atlas_reranker = None
    if args.aiproxy_url:
        raw_key = args.aiproxy_admin_key_file or os.environ.get("PLAYBOX_AIPROXY_ADMIN_KEY", "")
        if raw_key:
            aiproxy_client = AiproxyClient(args.aiproxy_url, load_admin_key(raw_key))
            aiproxy_client.ensure_group(args.aiproxy_group)
            if args.atlas_embed_model or args.atlas_rerank_model:
                aiproxy_client.ensure_group("atlas:embed")
                atlas_embed_tok = aiproxy_client.mint_worker_token(
                    "atlas:embed",
                    f"atlas-{tag[:20]}",
                    hard_usd_cap=max(0.05, args.budget * 0.1),
                    failover_policy=args.aiproxy_failover_policy,
                    cache_mode=args.aiproxy_cache_mode,
                    run_id=tag,
                    member_id="atlas",
                )
                cache_db = (
                    None
                    if args.aiproxy_cache_mode == "off"
                    else os.path.join(R.D, "atlas-embed-cache.sqlite")
                )
                req_hdrs = {
                    "X-Playbox-Run-Id": tag,
                    "X-Playbox-Member-Id": "atlas",
                    "X-Playbox-Cache-Mode": args.aiproxy_cache_mode,
                    "X-Playbox-Failover-Policy": args.aiproxy_failover_policy,
                }
                if args.atlas_embed_model:
                    atlas_embedder = AiproxyEmbedder(
                        args.aiproxy_url,
                        atlas_embed_tok.key,
                        model=args.atlas_embed_model,
                        cache_path=cache_db,
                        extra_headers=req_hdrs,
                    )
                if args.atlas_rerank_model:
                    atlas_reranker = AiproxyReranker(
                        args.aiproxy_url,
                        atlas_embed_tok.key,
                        model=args.atlas_rerank_model,
                        extra_headers=req_hdrs,
                    )

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
            embed_fn=atlas_embedder,
            rerank_fn=atlas_reranker,
        )
    )
    if atlas_embedder is not None:
        atlas_embedder.close()
    if aiproxy_client is not None and atlas_embed_tok is not None:
        aiproxy_client.revoke_token("atlas:embed", atlas_embed_tok.token_id)

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
        if args.repro_gate and rt_info["parser"] in ("django", "pytest"):
            repro_gate = run_repro_generator(
                r=r,
                rt_info=rt_info,
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
                harness=args.harness,
                worker_slot=str(args.worker_slot),
                aiproxy_client=aiproxy_client,
                aiproxy_group="bench:repro",
                aiproxy_hard_usd_cap=min(0.15, max(0.05, args.budget * 0.25)),
                aiproxy_run_id=tag,
                aiproxy_failover_policy=args.aiproxy_failover_policy,
                aiproxy_cache_mode=args.aiproxy_cache_mode,
            )

        worker_links = [next_link() for _ in range(args.workers)]
        w_models = [m.strip() for m in args.worker_models.split(",") if m.strip()] if args.worker_models else []
        per_worker_cap = max(0.05, args.budget / max(args.workers, 1))
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            futs = [
                pool.submit(
                    run_worker,
                    f"w{i}",
                    i,
                    inst=args.inst,
                    r=r,
                    rt_info=rt_info,
                    run_dir=run_dir,
                    host=host,
                    link=worker_links[i],
                    provider=args.provider,
                    model=w_models[i % len(w_models)] if w_models else args.model,
                    atlas_brief=atlas_brief,
                    atlas_index_path=atlas_idx_path,
                    timeout=args.timeout,
                    keep=not args.legacy_replay,
                    extra_guidance=args.extra_guidance,
                    approvers_hex=approvers_hex,
                    allow_egress=args.allow_egress,
                    harness=args.harness,
                    worker_slot=f"{args.worker_slot}-{i}",
                    aiproxy_client=aiproxy_client,
                    aiproxy_group=args.aiproxy_group,
                    aiproxy_hard_usd_cap=per_worker_cap,
                    aiproxy_run_id=tag,
                    aiproxy_failover_policy=args.aiproxy_failover_policy,
                    aiproxy_cache_mode=args.aiproxy_cache_mode,
                )
                for i in range(args.workers)
            ]
            cheap_candidates = [f.result() for f in futs]

        # Non-oracle online gate: existing tests + valid non-vacuous repro script (NEVER hidden SWE tests).
        repro_script = repro_gate.script if (repro_gate and repro_gate.valid) else ""

        def online_verify(patch: str):
            return _run_sealed_container_check(r, patch, rt_info=rt_info, repro_script=repro_script)

        all_workers_meta = [c.meta for c in cheap_candidates]
        escalate_fn = None
        if args.escalate_model:
            esc_link = next_link()
            esc_prov = args.escalate_provider or args.provider

            def _escalate(brief: str, remaining_usd: float) -> CandidateResult:
                cand = run_worker(
                    "w_escalate",
                    99,
                    inst=args.inst,
                    r=r,
                    rt_info=rt_info,
                    run_dir=run_dir,
                    host=host,
                    link=esc_link,
                    provider=esc_prov,
                    model=args.escalate_model,
                    atlas_brief=brief,
                    atlas_index_path=atlas_idx_path,
                    timeout=args.timeout,
                    keep=not args.legacy_replay,
                    extra_guidance="Prior attempts failed the independent check. Trace the core call chain carefully.",
                    approvers_hex=approvers_hex,
                    allow_egress=args.allow_egress,
                    harness=args.harness,
                    worker_slot=f"{args.worker_slot}-esc",
                    aiproxy_client=aiproxy_client,
                    aiproxy_group=args.aiproxy_group,
                    aiproxy_hard_usd_cap=max(0.05, remaining_usd),
                    aiproxy_run_id=tag,
                    aiproxy_failover_policy=args.aiproxy_failover_policy,
                    aiproxy_cache_mode=args.aiproxy_cache_mode,
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

        host_ledger = None
        gateway_total_cost: float | None = None
        if aiproxy_client is not None:
            try:
                host_ledger = aiproxy_client.fetch_ledger(run_id=tag)
                Path(args.outdir, f"{tag}.llm_ledger.json").write_text(
                    json.dumps(host_ledger, indent=2), encoding="utf-8"
                )
                gateway_total_cost = round(float((host_ledger.get("totals") or {}).get("cost_usd", 0.0) or 0.0), 6)
                atlas_cost = round(max(0.0, gateway_total_cost - float(decision.total_cost_usd)), 6)
                decision.provenance["atlas_embed_cost_usd"] = atlas_cost
                decision.provenance["total_cost_usd"] = gateway_total_cost
                decision.total_cost_usd = gateway_total_cost
            except Exception as exc:  # noqa: BLE001
                host_ledger = {"source": "client_unverified", "aiproxy_error": str(exc)[:200]}

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
            llm_ledger=host_ledger,
            signing_key=signing_key,
        )
        receipt_ok, receipt_msg = verify_signed_receipt(receipt, require_pq=True)

        # Gate-verified compounding memory: default `--memory-gate online` is strictly non-oracle
        # (uses `online_gate_passed`, NEVER hidden holdout `holdout_resolved`, during online sweeps).
        lesson_persisted = False
        online_ok = bool(winner and winner.passed)
        mem_gate_ok = online_ok if args.memory_gate == "online" else holdout_resolved
        if args.memory_file and mem_gate_ok and receipt_ok and win_patch.strip():
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
        default_attest = os.path.expanduser("~/.playbox/history/default.log")
        gate_bundle_verified = False
        gate_bundle_digest = ""
        gate_achieved_tier = ""
        gate_promotions = 0
        gate_pq_committed = 0
        exp_cmd = [
            R.PLAYBOX, "gate", "export",
            "--state", os.path.join(run_dir, "state"),
            "--receipt", str(rcpt_path),
            "--sign-key", key_file,
            "--out", str(bundle_path),
        ]
        if os.path.isfile(default_attest):
            exp_cmd.extend(["--attestation", default_attest])
        exp_proc = subprocess.run(
            exp_cmd,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if exp_proc.returncode == 0 and bundle_path.exists():
            vfy_cmd = [R.PLAYBOX, "gate", "verify", str(bundle_path), "--require-signature", "--require-pq", "--json"]
            if os.path.isfile(default_attest):
                vfy_cmd.extend(["--require-tier", "t1"])
            vfy_proc = subprocess.run(
                vfy_cmd,
                capture_output=True,
                text=True,
                timeout=30,
            )
            if vfy_proc.returncode == 0:
                try:
                    vfy_json = json.loads(vfy_proc.stdout)
                    gate_bundle_verified = bool(vfy_json.get("ok") and vfy_json.get("pq_signed"))
                    gate_bundle_digest = str(vfy_json.get("bundle_digest") or "")
                    gate_achieved_tier = str(vfy_json.get("achieved_tier") or "")
                    gate_promotions = int(vfy_json.get("promotions") or 0)
                    gate_pq_committed = int(vfy_json.get("room_log_pq_committed") or 0)
                except ValueError:
                    gate_bundle_verified = False

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
            "gate_achieved_tier": gate_achieved_tier,
            "gate_promotions": gate_promotions,
            "gate_pq_committed": gate_pq_committed,
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
        state_dst = Path(args.outdir, "states", tag)
        state_src = Path(run_dir, "state")
        if state_src.is_dir():
            state_dst.mkdir(parents=True, exist_ok=True)
            for fn in ("audit.log", "root.key", "endpoint.key", "egress.jsonl"):
                sp = state_src / fn
                if sp.is_file():
                    shutil.copy2(sp, state_dst / fn)
        shutil.rmtree(os.path.join(run_dir, "repo"), ignore_errors=True)


if __name__ == "__main__":
    main()
