#!/usr/bin/env python3
"""One Hermes run on a SWE-bench instance. usage: run_swe.py INST(flask|django|...) ARM(L|S) N OUTDIR
L: terminal.backend local, repo + native arm64 venv on the host. S: ssh into an OpenShell microVM (sandbox parity-INST[-sSLOT], arm64 image
pb-swe-INST:arm64 with the same repo and pinned deps). Scored by swe/score.py (apply agent patch minus tests/, plus hidden test_patch)."""
import json, os, re, shlex, shutil, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "swe"))
from score import extract_django_modules, load, score, _inst

def kill_home_gateways(home_dir: str) -> None:
    pids = subprocess.run(["pgrep", "-f", "hermes_cli.main.*gateway"], capture_output=True, text=True).stdout.split()
    for pid in pids:
        env_out = subprocess.run(["ps", "eww", "-p", pid], capture_output=True, text=True).stdout.split()
        if f"HERMES_HOME={home_dir}" in env_out:
            try:
                os.kill(int(pid), 15)
            except OSError:
                pass
    time.sleep(1)

def ensure_openshell_ssh_shim(base_dir: str, sb_name: str) -> str:
    ssh_dir = f"{base_dir}/ssh-{sb_name}"
    bin_dir = f"{ssh_dir}/bin"
    os.makedirs(bin_dir, exist_ok=True)
    cfg_path = f"{ssh_dir}/ssh_config"
    openshell_bin = shutil.which("openshell") or "/opt/homebrew/bin/openshell"
    with open(cfg_path, "w") as f:
        f.write(
            f"Host openshell-{sb_name}.default\n"
            f"    User sandbox\n"
            f"    StrictHostKeyChecking no\n"
            f"    UserKnownHostsFile /dev/null\n"
            f"    GlobalKnownHostsFile /dev/null\n"
            f"    LogLevel ERROR\n"
            f"    ServerAliveInterval 15\n"
            f"    ServerAliveCountMax 3\n"
            f"    ProxyCommand {openshell_bin} ssh-proxy --gateway-name openshell --name {sb_name} --workspace default\n"
        )
    for tool in ("ssh", "scp"):
        tp = f"{bin_dir}/{tool}"
        with open(tp, "w") as f:
            f.write(f"#!/bin/sh\nexec /usr/bin/{tool} -F {shlex.quote(cfg_path)} \"$@\"\n")
        os.chmod(tp, 0o755)
    return bin_dir

inst, arm, n, out = sys.argv[1:5]
dirty_git = "--dirty-git-history" in sys.argv[5:]  # opt-in only to reproduce pre-fix .git history leak
slot = os.environ.get("PB_SLOT", "").strip()
slot_sfx = f"-s{slot}" if slot else ""
H = os.path.expanduser("~/.playbox-hermes/openshell-parity"); SWE = os.environ.get("PB_SWE_DIR", f"{H}/swe")
r = load(inst)
_, img = _inst(inst)
fam = r.get("pb_family") or ("django" if inst.startswith("django-") else inst)
img_fam = "django" if "django" in img else ("flask" if "flask" in img else ("polyglot" if "polyglot" in img else fam))
SB = f"parity-{img_fam}{slot_sfx}"; tag = f"{inst}-{arm}{n}"
os.makedirs(out, exist_ok=True)
work = tempfile.mkdtemp(prefix=f"pb-{tag}-")
env = dict(os.environ)
GIT = ["git", "-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0", "-c", "user.name=bench", "-c", "user.email=bench@example.com"]
home = f"{work}/home" if os.environ.get("PB_AIPROXY_URL", "").strip() else f"{H}/home-{img_fam}-{arm}{slot_sfx}"
if not os.path.isdir(home):
    os.makedirs(home, exist_ok=True)
    skip = ("tools", "hermes-agent", "installs", "bin", "node", "desktop", "bootstrap-cache", "backups", "skills", "cache", "attachments", "image_cache",
            "audio_cache", "credentials", "config.yaml", "logs", "sessions", "state.db", "gateway", "kanban", "plugins", "hooks", "home", "locks", "lsp", "memories")
    for f in os.listdir(os.path.expanduser("~/.hermes")):
        if f.startswith(".") or f.startswith(skip): continue
        dst_link = f"{home}/{f}"
        if not os.path.lexists(dst_link):
            os.symlink(os.path.expanduser(f"~/.hermes/{f}"), dst_link)
for d in ("skills", "cache", "attachments"):   # the ssh backend syncs these into the VM; keep empty in both arms
    shutil.rmtree(f"{home}/{d}", ignore_errors=True); os.makedirs(f"{home}/{d}", exist_ok=True)
kill_home_gateways(home)
cfg = open(os.path.expanduser("~/.hermes/config.yaml")).read()
def setterm(cfg, **kv):
    head, rest = cfg.split("\nterminal:\n", 1); block, tail = re.split(r"\n(?=\S)", rest, maxsplit=1)
    for k, v in kv.items():
        block, c = re.subn(rf"(?m)^  {k}:.*$", f"  {k}: {v}", block)
        if not c: block += f"\n  {k}: {v}"
    return head + "\nterminal:\n" + block + "\n" + tail

if arm == "L":
    repo = f"{work}/repo"
    if r.get("pb_host_tar"):
        os.makedirs(repo, exist_ok=True)
        subprocess.run(["tar", "-xf", r["pb_host_tar"], "-C", repo], check=True)
        if not os.path.isdir(f"{repo}/.git"):
            subprocess.run([*GIT, "init", "-q", "-b", "main", repo], check=True)
            subprocess.run([*GIT, "-C", repo, "add", "-A"], check=True)
            subprocess.run([*GIT, "-C", repo, "commit", "-q", "-m", "base"], check=True)
    elif dirty_git:
        shutil.copytree(f"{SWE}/{fam}/testbed", repo, symlinks=True)
        subprocess.run(["git", "-C", repo, "checkout", "-q", "-f", r["base_commit"]], check=True)
    else:
        # Single-commit archive at base_commit: closes `git log --all` future-commit leakage.
        os.makedirs(repo, exist_ok=True)
        subprocess.run(f"git -C {SWE}/{fam}/testbed archive {r['base_commit']} | tar -x -C {repo}", shell=True, check=True)
        subprocess.run([*GIT, "init", "-q", "-b", "main", repo], check=True)
        subprocess.run([*GIT, "-C", repo, "add", "-A"], check=True)
        subprocess.run([*GIT, "-C", repo, "commit", "-q", "-m", "base"], check=True)
    py = r.get("pb_host_python") or f"{SWE}/venv-{fam}/bin/python"
    if os.path.exists(py) and fam in ("django", "flask"):
        subprocess.run(["uv", "pip", "install", "-q", "--python", py, "-e", repo, "--no-deps"], check=True)
    sh = lambda c, **k: subprocess.run(["bash", "-c", c], capture_output=True, text=True, **k)
    cfg = setterm(cfg, backend="local", cwd=repo)
else:
    repo = f"/sandbox/pb/{arm}{n}-{inst}/repo"; py = r.get("pb_python") or "/usr/local/venv/bin/python"
    ex = lambda c: subprocess.run(["openshell", "sandbox", "exec", SB, "--", "sh", "-c", c], capture_output=True, text=True)
    if ex("true").returncode != 0:
        subprocess.run(["openshell", "sandbox", "delete", SB], capture_output=True, text=True)
        cr = subprocess.run(["openshell", "sandbox", "create", "--name", SB, "--from", img, "--detach"], capture_output=True, text=True)
        if cr.returncode != 0 and ex("true").returncode != 0:
            time.sleep(2)
            if ex("true").returncode != 0:
                subprocess.run(["openshell", "sandbox", "delete", SB], capture_output=True, text=True)
                subprocess.run(["openshell", "sandbox", "create", "--name", SB, "--from", img, "--detach"], check=True, capture_output=True, text=True)
    ssh_bin = ensure_openshell_ssh_shim(H, SB)
    env["PATH"] = f"{ssh_bin}:" + env["PATH"]
    t0 = time.time()
    if r.get("pb_host_tar"):
        ex(f"rm -rf /sandbox/pb/{arm}{n}-{inst}; mkdir -p {repo}")
        up_tar = f"/sandbox/pb/{arm}{n}-{inst}/base.tar"
        up1 = subprocess.run(
            ["openshell", "sandbox", "upload", "--no-git-ignore", SB, r["pb_host_tar"], up_tar],
            capture_output=True, text=True,
        )
        assert up1.returncode == 0, f"upload failed: {up1.stderr or up1.stdout}"
        up2 = ex(
            f"tar -xf {up_tar} -C {repo} && rm -f {up_tar} && cd {repo} "
            f"&& printf '__pycache__/\\n*.pyc\\n*.pyo\\n._*\\n.hermes-tmp.*\\ntarget/\\n' >> .gitignore "
            f"&& git init -q -b main && git -c user.name=bench -c user.email=bench@example.com add -A "
            f"&& git -c user.name=bench -c user.email=bench@example.com commit -q -m base"
        )
        assert up2.returncode == 0, f"init failed: {up2.stderr or up2.stdout}"
    else:
        if dirty_git:
            setup_cmd = (
                f"rm -rf /sandbox/pb/{arm}{n}-*; mkdir -p /sandbox/pb/{arm}{n}-{inst} "
                f"&& cp -a /usr/local/testbed {repo} && cd {repo} && git checkout -q -f {r['base_commit']} && git status --short | head -1"
            )
        else:
            # Single-commit archive inside the VM so `git log --all` cannot leak future gold commits.
            setup_cmd = (
                f"rm -rf /sandbox/pb/{arm}{n}-*; mkdir -p {repo} "
                f"&& git -C /usr/local/testbed archive {r['base_commit']} | tar -x -C {repo} "
                f"&& cd {repo} && git init -q -b main "
                f"&& git -c user.name=bench -c user.email=bench@example.com add -A "
                f"&& git -c user.name=bench -c user.email=bench@example.com commit -q -m base "
                f"&& git status --short | head -1"
            )
        e = ex(setup_cmd); assert e.returncode == 0, e.stderr
    upload_s = time.time() - t0
    cfg = setterm(cfg, backend="ssh", cwd=repo, ssh_host=f"openshell-{SB}.default", ssh_user="sandbox", ssh_port="22")
    sh = lambda c, **k: ex(c)
open(f"{home}/config.yaml", "w").write(cfg); env["HERMES_HOME"] = home; env.pop("TERMINAL_ENV", None)
env["HERMES_DISABLE_LAZY_INSTALLS"] = "1"
for _shared in ("tools", "runtime", "installs"):
    _src = os.path.expanduser(f"~/.hermes/{_shared}")
    _dst = os.path.join(home, _shared)
    if os.path.exists(_src) and not os.path.lexists(_dst):
        os.symlink(_src, _dst)

aiproxy_url = os.environ.get("PB_AIPROXY_URL", "").strip()
aiproxy_key_file = os.environ.get("PB_AIPROXY_ADMIN_KEY_FILE", "").strip()
aiproxy_lease = None
aiproxy_run_id = f"{tag}-{os.getpid()}"
if aiproxy_url and os.environ.get("PB_HARNESS", "hermes") == "hermes":
    _repo_root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
    _hpb = os.path.join(_repo_root, "hermes-playbox-backend")
    if _hpb not in sys.path:
        sys.path.insert(0, _hpb)
    from playbox_router.aiproxy import AiproxyClient, WorkerTokenLease, load_admin_key
    _raw_key = aiproxy_key_file or os.environ.get("PLAYBOX_AIPROXY_ADMIN_KEY", "")
    if _raw_key:
        _ac = AiproxyClient(aiproxy_url, load_admin_key(_raw_key))
        aiproxy_lease = WorkerTokenLease(
            client=_ac,
            group="bench:arm-s",
            worker_label=f"arm-s-{inst[:16]}",
            hard_usd_cap=float(os.environ.get("PB_BUDGET", "1.50")),
            run_id=aiproxy_run_id,
            task_id=inst,
            failover_policy="same_model_only",
            cache_mode="off",
        )
        aiproxy_lease.__enter__()
        env.update(aiproxy_lease.env_overrides())
        _auth_p = os.path.join(home, "auth.json")
        if os.path.lexists(_auth_p):
            try:
                os.remove(_auth_p)
            except OSError:
                pass
        subprocess.run(
            [
                os.path.expanduser("~/.hermes/hermes-agent/venv/bin/python"), "-c",
                "import os,yaml,sys; p=sys.argv[1]; c=yaml.safe_load(open(p)) or {}; "
                "c.pop('providers',None); c.pop('custom_providers',None); c['fallback_providers']=[]; "
                "c['model']={'default': os.environ.get('PB_MODEL',''), 'provider': os.environ.get('PB_PROVIDER','custom')}; "
                "open(p,'w').write(yaml.safe_dump(c))",
                f"{home}/config.yaml",
            ],
            env=env,
            check=True,
        )

parser = r.get("pb_parser") or ("pytest" if inst == "flask" else "django")
mods = extract_django_modules(r["F"]) if parser == "django" else []
mod = mods[0] if mods else ""
pp = f"{repo}/src" if inst == "flask" else repo
if r.get("pb_test_hint"):
    test_hint = r["pb_test_hint"].format(repo=repo, py=py)
elif parser == "pytest":
    tfile = r["F"][0].split("::")[0] if r.get("F") else "tests"
    test_hint = f"PYTHONPATH={pp} {py} -m pytest {tfile} -q"
else:
    test_hint = f"cd tests && PYTHONPATH={pp} {py} runtests.py --settings=test_sqlite --parallel 1 {mod}"
prompt = (f"The repository in {repo} has a bug. Fix it in the source code so the behaviour described in the issue is correct, without breaking existing behaviour. "
          f"Use the runtime at {py} (all dependencies are installed). Example test command from the repo root: `{test_hint}`. "
          f"Do not edit existing tests; hidden tests will check your fix. Edit files in place, then reply with a one-line summary.\n\nISSUE:\n{r['problem_statement']}")
usage = f"{out}/{tag}.usage.json"
harness = os.environ.get("PB_HARNESS", "hermes")
model = os.environ.get("PB_MODEL", "claude-haiku-4-5-20251001")
provider = os.environ.get("PB_PROVIDER", "anthropic")
timeout_s = int(os.environ.get("PB_TIMEOUT", "900"))
if harness in ("agy", "devin"):
    agy_ws = f"{work}/agy_ws"
    os.makedirs(f"{agy_ws}/.devin", exist_ok=True)
    with open(f"{agy_ws}/.devin/mcp_config.local.json", "w") as mf:
        json.dump({"mcpServers": {"playbox": {"disabled": True}}}, mf)
    if arm == "S":
        osh_dir = f"{work}/osh_bin"
        os.makedirs(osh_dir, exist_ok=True)
        osh_path = f"{osh_dir}/osh"
        with open(osh_path, "w") as f:
            f.write(
                f"#!/usr/bin/env python3\n"
                f"import os, shlex, subprocess, sys, tempfile\n"
                f"SB = {SB!r}\n"
                f"REPO = {repo!r}\n"
                f"if len(sys.argv) >= 3 and sys.argv[1] == '--write':\n"
                f"    data = sys.stdin.buffer.read()\n"
                f"    with tempfile.NamedTemporaryFile(delete=False) as tf:\n"
                f"        tf.write(data)\n"
                f"        tmp_path = tf.name\n"
                f"    try:\n"
                f"        p = subprocess.run(['openshell', 'sandbox', 'upload', '--no-git-ignore', SB, tmp_path, sys.argv[2]])\n"
                f"    finally:\n"
                f"        os.unlink(tmp_path)\n"
                f"    raise SystemExit(p.returncode)\n"
                f"cmd = ' '.join(sys.argv[1:])\n"
                f"p = subprocess.run(['openshell', 'sandbox', 'exec', SB, '--', 'sh', '-c', f'cd {{shlex.quote(REPO)}} && {{cmd}}'])\n"
                f"raise SystemExit(p.returncode)\n"
            )
        os.chmod(osh_path, 0o755)
        env["PATH"] = f"{osh_dir}:" + env["PATH"]
        agy_prompt = (
            f"The target repository is inside an OpenShell sandbox at `{repo}` (NOT in your local directory). "
            f"Run EVERY command inside the sandbox via `osh \"<command>\"` (which starts in `{repo}`, e.g. `osh \"rg -n pattern .\"`, "
            f"`osh \"sed -n '100,160p' path.py\"`), and write/update files inside `{repo}` via `osh --write {repo}/path.py <<'EOF' ... EOF` "
            f"or inline python (`osh \"{py} -c '...'\"`). "
            f"Fix the bug in the source code under `{repo}` so the behaviour described in the issue is correct, without breaking existing behaviour. "
            f"Use the Python runtime at `{py}` (all dependencies are installed). Example test command: `osh \"{test_hint}\"`. "
            f"Do not edit existing tests; hidden tests will check your fix. Edit source files in place inside `{repo}`, then reply with a one-line summary.\n\nISSUE:\n{r['problem_statement']}"
        )
        run_cwd = agy_ws
    else:
        agy_prompt = prompt
        run_cwd = repo
    env.pop("ACP_BACKEND", None)
    if harness == "agy":
        agy_bin = os.environ.get("AGY_BIN", os.path.expanduser("~/.local/bin/agy"))
        cmd = [agy_bin, "--model", model, "--output-format", "json", "--dangerously-skip-permissions", "--disable-slash-commands", "-p", agy_prompt]
    else:
        devin_bin = os.environ.get("DEVIN_BIN", os.path.expanduser("~/.local/bin/devin"))
        cmd = [devin_bin, "--model", model, "--permission-mode", "dangerous", "--respect-workspace-trust", "false", "--process-model", "in-process", "--verbosity", "1", "-p", agy_prompt]
    t0 = time.time()
    try:
        p = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=timeout_s, cwd=run_cwd)
        rc, stdout, stderr = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired as exc:
        rc = 124
        stdout = (exc.stdout or b"").decode() if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        stderr = "timeout"
    wall = time.time() - t0
    if harness == "devin":
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from run_swe_playbox_v2 import _extract_devin_usage
        u_out = _extract_devin_usage(run_cwd, t0, model)
        if rc != 0 and not u_out.get("input_tokens"):
            u_out["failed"] = True
            u_out["failure"] = (stderr or stdout)[-250:]
        json.dump(u_out, open(usage, "w"), indent=2)
    else:
        agy_lines = [ln.strip() for ln in (stdout or "").splitlines() if ln.strip().startswith("{")]
        if agy_lines:
            try:
                aj = json.loads(agy_lines[-1])
                au = aj.get("usage") or {}
                u_out = {
                    "model": model,
                    "provider": "agy",
                    "conversation_id": aj.get("conversation_id", ""),
                    "api_calls": int(aj.get("num_turns") or 1),
                    "input_tokens": int(au.get("input_tokens") or 0),
                    "output_tokens": int(au.get("output_tokens") or 0),
                    "thinking_tokens": int(au.get("thinking_tokens") or 0),
                    "cache_read_tokens": int(au.get("cache_read_tokens") or 0),
                    "total_tokens": int(au.get("total_tokens") or 0),
                    "failed": aj.get("status") not in ("SUCCESS", None) and rc != 0,
                    "failure": str(aj.get("error") or stderr[:200]) if (aj.get("status") not in ("SUCCESS", None) and rc != 0) else "",
                }
                json.dump(u_out, open(usage, "w"), indent=2)
            except ValueError:
                pass
else:
    cmd = [os.path.expanduser("~/.local/bin/hermes"), "--usage-file", usage, "-z", prompt, "-m", model, "--provider", provider, "-t", "terminal,file", "--yolo"]
    t0 = time.time()
    rc, stdout, stderr = 1, "", ""
    for _gw_try in range(3):
        try:
            p = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=timeout_s, cwd=tempfile.gettempdir())
            rc, stdout, stderr = p.returncode, p.stdout, p.stderr
        except subprocess.TimeoutExpired as exc:
            rc = 124
            stdout = (exc.stdout or b"").decode() if isinstance(exc.stdout, bytes) else (exc.stdout or "")
            stderr = "timeout"
            break
        if "Gateway draining" not in (stdout + stderr):
            break
        time.sleep(3.0)
    wall = time.time() - t0
if aiproxy_lease is not None:
    aiproxy_lease.__exit__(None, None, None)
kill_home_gateways(home)
if arm == "S":
    subprocess.run(["pkill", "-f", f"openshell-{SB}\\.default"])
open(f"{out}/{tag}.log", "w").write(stdout + "\n--- stderr ---\n" + stderr)
d = sh(f"cd {repo} && git add -A -- . ':!tests' ':!test' ':!testing' ':!*.pyc' ':!*.so' ':!target' && git diff --cached HEAD -- . ':!tests' ':!test' ':!testing' ':!*.pyc' ':!*.so' ':!target'")
raw_blocks = re.split(r"(?m)^(?=diff --git )", d.stdout or "")
patch = "".join(
    b for b in raw_blocks
    if b.strip()
    and not re.search(r"^diff --git a/(?:.+/)?(?:tests?|testing)/", b)
    and "\nBinary files " not in b
)
u_data = json.load(open(usage)) if os.path.exists(usage) else {}
host_cost = aiproxy_lease.host_cost_usd if aiproxy_lease is not None else None
if host_cost is not None:
    u_data["estimated_cost_usd"] = round(float(host_cost), 6)
    u_data["cost_source"] = "host_aiproxy"
elif not float(u_data.get("estimated_cost_usd") or 0.0) and (u_data.get("input_tokens") or u_data.get("output_tokens")):
    inp = int(u_data.get("input_tokens") or 0)
    cached = int(u_data.get("cache_read_tokens") or 0)
    out_tok = int(u_data.get("output_tokens") or 0)
    m_low = model.lower()
    if "opus-5-5" in m_low or "opus-5.5" in m_low:
        ir, cr, or_ = 4.00, 0.20, 20.00
    elif "neon" in m_low:
        ir, cr, or_ = 3.00, 0.15, 15.00
    elif any(k in m_low for k in ("sonnet", "terra", "opus", "gpt-5.4")):
        ir, cr, or_ = 3.00, 0.30, 15.00
    else:
        ir, cr, or_ = 1.00, 0.10, 5.00
    u_data["estimated_cost_usd"] = round((max(0, inp - cached) * ir + cached * cr + out_tok * or_) / 1_000_000.0, 6)
res = dict(inst=inst, arm=arm, n=n, model=model, provider=provider, dirty_git=dirty_git, wall_s=round(wall, 1), rc=rc, patch_lines=len(patch.splitlines()), usage=u_data, llm_ledger_source=("host_aiproxy" if host_cost is not None else "client_estimate"))
res.update(score(inst, patch)); res.pop("tail", None); res.pop("ok", None)
if arm == "S": res["setup_s"] = round(upload_s, 1)
json.dump(res, open(f"{out}/{tag}.json", "w"), indent=1); open(f"{out}/{tag}.patch", "w").write(patch)
print(json.dumps({k: v for k, v in res.items() if k != "usage"}), res["usage"].get("estimated_cost_usd"))
if arm == "S":
    sh(f"rm -rf /sandbox/pb/{arm}{n}-{inst}")
shutil.rmtree(work, ignore_errors=True)
