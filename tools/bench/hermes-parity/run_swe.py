#!/usr/bin/env python3
"""One Hermes run on a SWE-bench instance. usage: run_swe.py INST(flask|django) ARM(L|S) N OUTDIR
L: terminal.backend local, repo + native arm64 venv on the host. S: ssh into an OpenShell microVM (sandbox parity-INST, arm64 image
pb-swe-INST:arm64 with the same repo and pinned deps). Scored by swe/score.py (apply agent patch minus tests/, plus hidden test_patch)."""
import json, os, re, shutil, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "swe"))
from score import load, score

inst, arm, n, out = sys.argv[1:5]
H = os.path.expanduser("~/.playbox-hermes/openshell-parity"); SWE = f"{H}/swe"
SB = f"parity-{inst}"; r = load(inst); tag = f"{inst}-{arm}{n}"
os.makedirs(out, exist_ok=True)
work = tempfile.mkdtemp(prefix=f"pb-{tag}-")
env = dict(os.environ)
home = f"{H}/home-{inst}-{arm}"          # persistent per arm/instance (Hermes builds tools/ there on first use); only config.yaml changes
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
    repo = f"{work}/repo"; shutil.copytree(f"{SWE}/{inst}/testbed", repo, symlinks=True)
    py = f"{SWE}/venv-{inst}/bin/python"
    subprocess.run(["uv", "pip", "install", "-q", "--python", py, "-e", repo, "--no-deps"], check=True)
    sh = lambda c, **k: subprocess.run(["bash", "-c", c], capture_output=True, text=True, **k)
    cfg = setterm(cfg, backend="local", cwd=repo)
else:
    repo = f"/sandbox/pb/{arm}{n}-{inst}/repo"; py = "/usr/local/venv/bin/python"
    env["PATH"] = f"{H}/ssh-{SB}/bin:" + env["PATH"]
    ex = lambda c: subprocess.run(["openshell", "sandbox", "exec", SB, "--", "sh", "-c", c], capture_output=True, text=True)
    t0 = time.time(); e = ex(f"rm -rf /sandbox/pb/{arm}{n}-{inst}; mkdir -p /sandbox/pb/{arm}{n}-{inst} && cp -a /usr/local/testbed {repo} && cd {repo} && git status --short | head -1"); assert e.returncode == 0, e.stderr
    upload_s = time.time() - t0
    cfg = setterm(cfg, backend="ssh", cwd=repo, ssh_host=f"openshell-{SB}.default", ssh_user="sandbox", ssh_port="22")
    sh = lambda c, **k: ex(c)
open(f"{home}/config.yaml", "w").write(cfg); env["HERMES_HOME"] = home; env.pop("TERMINAL_ENV", None)

pp = f"{repo}/src" if inst == "flask" else repo
test_hint = f"PYTHONPATH={pp} {py} -m pytest tests/test_blueprints.py -q" if inst == "flask" else f"cd tests && PYTHONPATH={pp} {py} runtests.py --settings=test_sqlite --parallel 1 view_tests.tests.test_debug"
prompt = (f"The repository in {repo} has a bug. Fix it in the source code so the behaviour described in the issue is correct, without breaking existing behaviour. "
          f"Use the Python at {py} (all dependencies are installed). Example test command from the repo root: `{test_hint}`. "
          f"Do not edit existing tests; hidden tests will check your fix. Edit files in place, then reply with a one-line summary.\n\nISSUE:\n{r['problem_statement']}")
usage = f"{out}/{tag}.usage.json"
cmd = [os.path.expanduser("~/.local/bin/hermes"), "--usage-file", usage, "-z", prompt, "-m", "claude-haiku-4-5-20251001", "--provider", "anthropic", "-t", "terminal,file", "--yolo"]
t0 = time.time()
p = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=1800, cwd=tempfile.gettempdir())
wall = time.time() - t0
subprocess.run(["pkill", "-f", home]); subprocess.run(["pkill", "-f", "hermes-ssh/.*sock"])
open(f"{out}/{tag}.log", "w").write(p.stdout + "\n--- stderr ---\n" + p.stderr)
d = sh(f"cd {repo} && git add -A -- . ':!tests' && git diff --cached HEAD -- . ':!tests'")
patch = d.stdout
res = dict(inst=inst, arm=arm, n=n, wall_s=round(wall, 1), rc=p.returncode, patch_lines=len(patch.splitlines()), usage=json.load(open(usage)) if os.path.exists(usage) else {})
res.update(score(inst, patch)); res.pop("tail", None); res.pop("ok", None)
if arm == "S": res["setup_s"] = round(upload_s, 1)
json.dump(res, open(f"{out}/{tag}.json", "w"), indent=1); open(f"{out}/{tag}.patch", "w").write(patch)
print(json.dumps({k: v for k, v in res.items() if k != "usage"}), res["usage"].get("estimated_cost_usd"))
shutil.rmtree(work, ignore_errors=True)
