#!/usr/bin/env python3
"""One Hermes run on a SWE-bench instance. usage: run_swe.py INST(flask|django) ARM(L|S) N OUTDIR
L: terminal.backend local, repo + native arm64 venv on the host. S: ssh into an OpenShell microVM (sandbox parity-INST, arm64 image
pb-swe-INST:arm64 with the same repo and pinned deps). Scored by swe/score.py (apply agent patch minus tests/, plus hidden test_patch)."""
import json, os, re, shutil, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "swe"))
from score import load, score

inst, arm, n, out = sys.argv[1:5]
dirty_git = "--dirty-git-history" in sys.argv[5:]  # opt-in only to reproduce pre-fix .git history leak
H = os.path.expanduser("~/.playbox-hermes/openshell-parity"); SWE = f"{H}/swe"
fam = "django" if inst.startswith("django-") else inst
SB = f"parity-{fam}"; r = load(inst); tag = f"{inst}-{arm}{n}"
os.makedirs(out, exist_ok=True)
work = tempfile.mkdtemp(prefix=f"pb-{tag}-")
env = dict(os.environ)
GIT = ["git", "-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0", "-c", "user.name=bench", "-c", "user.email=bench@example.com"]
home = f"{H}/home-{fam}-{arm}"          # persistent per arm/instance (Hermes builds tools/ there on first use); only config.yaml changes
if not os.path.isdir(home):
    os.makedirs(home)
    skip = ("tools", "hermes-agent", "installs", "bin", "node", "desktop", "bootstrap-cache", "backups", "skills", "cache", "attachments", "image_cache",
            "audio_cache", "credentials", "config.yaml", "logs", "sessions", "state.db", "gateway", "kanban", "plugins", "hooks", "home", "locks", "lsp", "memories")
    for f in os.listdir(os.path.expanduser("~/.hermes")):
        if f.startswith(".") or f.startswith(skip): continue
        os.symlink(os.path.expanduser(f"~/.hermes/{f}"), f"{home}/{f}")
for d in ("skills", "cache", "attachments"):   # the ssh backend syncs these into the VM; keep empty in both arms
    shutil.rmtree(f"{home}/{d}", ignore_errors=True); os.makedirs(f"{home}/{d}")
subprocess.run(["pkill", "-f", home]); time.sleep(1)
cfg = open(os.path.expanduser("~/.hermes/config.yaml")).read()
def setterm(cfg, **kv):
    head, rest = cfg.split("\nterminal:\n", 1); block, tail = re.split(r"\n(?=\S)", rest, 1)
    for k, v in kv.items():
        block, c = re.subn(rf"(?m)^  {k}:.*$", f"  {k}: {v}", block)
        if not c: block += f"\n  {k}: {v}"
    return head + "\nterminal:\n" + block + "\n" + tail

if arm == "L":
    repo = f"{work}/repo"
    if dirty_git:
        shutil.copytree(f"{SWE}/{fam}/testbed", repo, symlinks=True)
        subprocess.run(["git", "-C", repo, "checkout", "-q", "-f", r["base_commit"]], check=True)
    else:
        # Single-commit archive at base_commit: closes `git log --all` future-commit leakage.
        os.makedirs(repo, exist_ok=True)
        subprocess.run(f"git -C {SWE}/{fam}/testbed archive {r['base_commit']} | tar -x -C {repo}", shell=True, check=True)
        subprocess.run([*GIT, "init", "-q", "-b", "main", repo], check=True)
        subprocess.run([*GIT, "-C", repo, "add", "-A"], check=True)
        subprocess.run([*GIT, "-C", repo, "commit", "-q", "-m", "base"], check=True)
    py = f"{SWE}/venv-{fam}/bin/python"
    subprocess.run(["uv", "pip", "install", "-q", "--python", py, "-e", repo, "--no-deps"], check=True)
    sh = lambda c, **k: subprocess.run(["bash", "-c", c], capture_output=True, text=True, **k)
    cfg = setterm(cfg, backend="local", cwd=repo)
else:
    repo = f"/sandbox/pb/{arm}{n}-{inst}/repo"; py = "/usr/local/venv/bin/python"
    env["PATH"] = f"{H}/ssh-{SB}/bin:" + env["PATH"]
    ex = lambda c: subprocess.run(["openshell", "sandbox", "exec", SB, "--", "sh", "-c", c], capture_output=True, text=True)
    if ex("true").returncode != 0:
        subprocess.run(["openshell", "sandbox", "delete", SB], capture_output=True, text=True)
        subprocess.run(["openshell", "sandbox", "create", "--name", SB, "--from", f"pb-swe-{fam}:arm64", "--detach"], check=True, capture_output=True, text=True)
    if dirty_git:
        setup_cmd = (
            f"rm -rf /sandbox/pb/{arm}{n}-{inst}; mkdir -p /sandbox/pb/{arm}{n}-{inst} "
            f"&& cp -a /usr/local/testbed {repo} && cd {repo} && git checkout -q -f {r['base_commit']} && git status --short | head -1"
        )
    else:
        # Single-commit archive inside the VM so `git log --all` cannot leak future gold commits.
        setup_cmd = (
            f"rm -rf /sandbox/pb/{arm}{n}-{inst}; mkdir -p {repo} "
            f"&& git -C /usr/local/testbed archive {r['base_commit']} | tar -x -C {repo} "
            f"&& cd {repo} && git init -q -b main "
            f"&& git -c user.name=bench -c user.email=bench@example.com add -A "
            f"&& git -c user.name=bench -c user.email=bench@example.com commit -q -m base "
            f"&& git status --short | head -1"
        )
    t0 = time.time(); e = ex(setup_cmd); assert e.returncode == 0, e.stderr
    upload_s = time.time() - t0
    cfg = setterm(cfg, backend="ssh", cwd=repo, ssh_host=f"openshell-{SB}.default", ssh_user="sandbox", ssh_port="22")
    sh = lambda c, **k: ex(c)
open(f"{home}/config.yaml", "w").write(cfg); env["HERMES_HOME"] = home; env.pop("TERMINAL_ENV", None)

mod = re.search(r"\((.+)\)", r["F"][0]).group(1).rsplit(".", 1)[0] if inst != "flask" else ""
pp = f"{repo}/src" if inst == "flask" else repo
test_hint = f"PYTHONPATH={pp} {py} -m pytest tests/test_blueprints.py -q" if inst == "flask" else f"cd tests && PYTHONPATH={pp} {py} runtests.py --settings=test_sqlite --parallel 1 {mod}"
prompt = (f"The repository in {repo} has a bug. Fix it in the source code so the behaviour described in the issue is correct, without breaking existing behaviour. "
          f"Use the Python at {py} (all dependencies are installed). Example test command from the repo root: `{test_hint}`. "
          f"Do not edit existing tests; hidden tests will check your fix. Edit files in place, then reply with a one-line summary.\n\nISSUE:\n{r['problem_statement']}")
usage = f"{out}/{tag}.usage.json"
model = os.environ.get("PB_MODEL", "claude-haiku-4-5-20251001")
provider = os.environ.get("PB_PROVIDER", "anthropic")
cmd = [os.path.expanduser("~/.local/bin/hermes"), "--usage-file", usage, "-z", prompt, "-m", model, "--provider", provider, "-t", "terminal,file", "--yolo"]
t0 = time.time()
p = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=1800, cwd=tempfile.gettempdir())
wall = time.time() - t0
subprocess.run(["pkill", "-f", home]); subprocess.run(["pkill", "-f", "hermes-ssh/.*sock"])
open(f"{out}/{tag}.log", "w").write(p.stdout + "\n--- stderr ---\n" + p.stderr)
d = sh(f"cd {repo} && git add -A -- . ':!tests' && git diff --cached HEAD -- . ':!tests'")
patch = d.stdout
u_data = json.load(open(usage)) if os.path.exists(usage) else {}
if not float(u_data.get("estimated_cost_usd") or 0.0) and (u_data.get("input_tokens") or u_data.get("output_tokens")):
    inp = int(u_data.get("input_tokens") or 0)
    cached = int(u_data.get("cache_read_tokens") or 0)
    out_tok = int(u_data.get("output_tokens") or 0)
    u_data["estimated_cost_usd"] = round((max(0, inp - cached) * 1.00 + cached * 0.10 + out_tok * 5.00) / 1_000_000.0, 6)
res = dict(inst=inst, arm=arm, n=n, model=model, provider=provider, dirty_git=dirty_git, wall_s=round(wall, 1), rc=p.returncode, patch_lines=len(patch.splitlines()), usage=u_data)
res.update(score(inst, patch)); res.pop("tail", None); res.pop("ok", None)
if arm == "S": res["setup_s"] = round(upload_s, 1)
json.dump(res, open(f"{out}/{tag}.json", "w"), indent=1); open(f"{out}/{tag}.patch", "w").write(patch)
print(json.dumps({k: v for k, v in res.items() if k != "usage"}), res["usage"].get("estimated_cost_usd"))
shutil.rmtree(work, ignore_errors=True)
