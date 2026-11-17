#!/usr/bin/env python3
"""One Hermes run on the Tripwire task. usage: run.py ARM(L|S) N OUTDIR
Arm L: terminal.backend local. Arm S: terminal.backend ssh into an OpenShell microVM (sandbox parity-s);
the model call and OAuth credential stay on the host. Each run gets its own HERMES_HOME (symlinks to the real one,
plus a rewritten config.yaml: terminal.backend local or ssh), because config.yaml overrides TERMINAL_* env vars and the real config must not change."""
import json, os, shutil, subprocess, sys, time, tempfile
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tripwire"))
from task import make_task, score_tree

arm, n, out = sys.argv[1], sys.argv[2], sys.argv[3]
HOME = os.path.expanduser("~/.playbox-hermes/openshell-parity")
SB, REMOTE_BASE = "parity-s", "/sandbox/pb"
os.makedirs(out, exist_ok=True)
work = tempfile.mkdtemp(prefix=f"pb-{arm}{n}-")
repo = make_task(work)
hidden = os.path.join(work, "hidden")
env = dict(os.environ)
SRC = os.path.expanduser("~/.hermes")
subprocess.run(["pkill", "-f", f"openshell-parity/homes/{arm}{n}/"]); time.sleep(1)
hh = os.path.join(HOME, "homes", f"{arm}{n}"); shutil.rmtree(hh, ignore_errors=True); os.makedirs(hh)
for f in os.listdir(SRC):                      # symlink everything (incl. credentials) except the config, which we rewrite
    if f.startswith(".") or f in ("skills", "cache", "attachments", "image_cache", "audio_cache", "credentials", "config.yaml", "logs", "sessions", "state.db", "state.db-shm", "state.db-wal") or f.startswith(("config.yaml", "gateway", "kanban")):
        continue
    os.symlink(os.path.join(SRC, f), os.path.join(hh, f))
for d in ("skills", "cache", "attachments"):   # empty in BOTH arms: the ssh backend would otherwise sync these host files into the VM
    os.makedirs(os.path.join(hh, d))
import re
cfg = open(os.path.join(SRC, "config.yaml")).read()
def setterm(cfg, **kv):
    head, rest = cfg.split("\nterminal:\n", 1)
    block, tail = re.split(r"\n(?=\S)", rest, 1)
    for k, v in kv.items():
        block, c = re.subn(rf"(?m)^  {k}:.*$", f"  {k}: {v}", block)
        if not c: block += f"\n  {k}: {v}"
    return head + "\nterminal:\n" + block + "\n" + tail
if arm == "L":
    cwd = repo
    cfg = setterm(cfg, backend="local", cwd=repo)
else:
    cwd = f"{REMOTE_BASE}/{arm}{n}/repo"
    cfg = setterm(cfg, backend="ssh", cwd=cwd, ssh_host="openshell-parity-s.default", ssh_user="sandbox", ssh_port="22")
    env["PATH"] = f"{HOME}/bin:" + env["PATH"]
    t0 = time.time()
    ex = lambda c: subprocess.run(["openshell", "sandbox", "exec", SB, "--", "sh", "-c", c], check=True)
    ex(f"rm -rf {REMOTE_BASE}/{arm}{n}; mkdir -p {REMOTE_BASE}/{arm}{n}")
    subprocess.run(["openshell", "sandbox", "upload", SB, repo, f"{REMOTE_BASE}/{arm}{n}"], check=True)
    ex(f"cd {cwd} && git init -q -b main && git add -A && git -c user.name=o -c user.email=o@x commit -qm base")
    upload_s = time.time() - t0
open(os.path.join(hh, "config.yaml"), "w").write(cfg)
env["HERMES_HOME"] = hh
env.pop("TERMINAL_ENV", None)
prompt = (f"The Python package `mini` in {cwd} has several bugs. Fix every bug so each function matches its docstring. "
          f"Run the visible tests with `cd {cwd} && python3 -m unittest discover -s tests`. The visible tests do not cover everything, "
          f"so read each module's docstring carefully and check edge cases. Edit the files in place; reply with a one-line summary when done.")
usage = os.path.join(out, f"{arm}{n}.usage.json")
cmd = [os.path.expanduser("~/.local/bin/hermes"), "--usage-file", usage, "-z", prompt, "-m", "claude-haiku-4-5-20251001",
       "--provider", "anthropic", "-t", "terminal,file", "--yolo"]
t0 = time.time()
p = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=1500, cwd=tempfile.gettempdir())
wall = time.time() - t0
subprocess.run(["pkill", "-f", f"openshell-parity/homes/{arm}{n}/"])   # hermes leaves a per-home daemon behind
subprocess.run(["pkill", "-f", "hermes-ssh/.*sock"])
open(os.path.join(out, f"{arm}{n}.log"), "w").write(p.stdout + "\n--- stderr ---\n" + p.stderr)
if arm == "S":
    dl = os.path.join(work, "dl"); os.makedirs(dl)
    subprocess.run(["openshell", "sandbox", "download", SB, f"{REMOTE_BASE}/{arm}{n}/repo", dl], check=True)
    tree = dl if os.path.isdir(os.path.join(dl, "mini")) else os.path.join(dl, "repo")
else:
    tree = repo
passed, total = score_tree(tree, hidden)
u = json.load(open(usage)) if os.path.exists(usage) else {}
res = dict(arm=arm, n=n, passed=passed, total=total, wall_s=round(wall, 1), rc=p.returncode, usage=u)
if arm == "S": res["upload_s"] = round(upload_s, 1)
json.dump(res, open(os.path.join(out, f"{arm}{n}.json"), "w"), indent=1)
print(json.dumps(res))
shutil.rmtree(work, ignore_errors=True)
