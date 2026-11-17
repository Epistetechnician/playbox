#!/usr/bin/env python3
"""Arm P of the SWE parity harness: Hermes (host, terminal backend `playbox`) -> Playbox room -> OpenShell microVM per command.
usage: run_swe_playbox.py INST(django-NNNNN) N OUTDIR
Same model, toolset and issue text as run_swe.py arms L and S. The room's host repo is the instance's Django tree at base_commit (single commit on
`main`, built once and cached); every delegate checks out `base`=main in a warm sandbox of image pb-swe-django:arm64 (non-root, no network).
The agent patch is the final delegate patch (diff vs base after the replayed edits), minus tests/; scoring is swe/score.py, as for L and S.
Env: PB_DIR (default ~/.playbox-hermes/parity-playbox, shares home-P with the easy-task run), PB_TIMEOUT (1800)."""
import importlib.util, json, os, re, shutil, subprocess, sys, time

H = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(H, "swe"))
spec = importlib.util.spec_from_file_location("pbrun", os.path.join(H, "..", "hermes-parity-playbox", "run.py"))
R = importlib.util.module_from_spec(spec); spec.loader.exec_module(R)
from score import load, score  # noqa: E402

inst, n, out = sys.argv[1:4]
IMG = "pb-swe-django:arm64"
R.IMAGE = IMG
SWE = os.path.expanduser("~/.playbox-hermes/openshell-parity/swe")
D = R.D
TIMEOUT = int(os.environ.get("PB_TIMEOUT", "1800"))
r = load(inst); tag = f"{inst}-P{n}"
os.makedirs(out, exist_ok=True)
GIT = ["git", "-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0", "-c", "user.name=bench", "-c", "user.email=bench@example.com"]


def host_repo(dst):
    """Cached single-commit repo of the Django tree at base_commit (branch main); copied per run."""
    cache = os.path.join(D, "swe-repos", inst)
    if not os.path.isdir(os.path.join(cache, ".git")):
        os.makedirs(cache, exist_ok=True)
        tb = f"{SWE}/django/testbed"
        subprocess.run(f"git -C {tb} archive {r['base_commit']} | tar -x -C {cache}", shell=True, check=True)
        subprocess.run([*GIT, "init", "-q", "-b", "main", cache], check=True)
        subprocess.run([*GIT, "-C", cache, "add", "-A"], check=True)
        subprocess.run([*GIT, "-C", cache, "commit", "-q", "-m", "base"], check=True)
    shutil.copytree(cache, dst, symlinks=True)


def strip_tests(patch):
    blocks = re.split(r"(?m)^(?=diff --git )", patch)
    return "".join(b for b in blocks if b.strip() and not re.match(r"diff --git a/tests/", b))


def one_attempt(attempt):
    idx = R.budget()
    d = os.path.join(D, f"swe-{tag}" + (f"-r{attempt}" if attempt else ""))
    shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
    host_repo(os.path.join(d, "repo"))
    t_setup = time.time()
    host = R.Host(d); host.hub_register()
    hh = os.path.join(D, "home-P")
    R.make_home(hh, "P", os.path.join(R.REPO, "hermes-playbox-backend", "playbox_terminal"))
    lf = os.path.join(d, "link.txt"); open(lf, "w").write(host.links[1]); os.chmod(lf, 0o600)
    env = {**os.environ, "HERMES_HOME": hh, "PYTHONUNBUFFERED": "1", "PYTHONPATH": os.path.expanduser("~/.hermes/hermes-agent")}
    for k in ("PLAYBOX_HOME", "PLAYBOX_HOSTING_DIR"): env.pop(k, None)
    usage = os.path.join(d, "usage.json")
    env.update(PLAYBOX_BIN=R.PLAYBOX, PLAYBOX_HOME=os.path.join(d, "mhome"), PLAYBOX_LINK_FILE=lf, PLAYBOX_IMAGE=IMG,
               PLAYBOX_STATS=os.path.join(d, "delegates.jsonl"), PLAYBOX_MCP_LOG=os.path.join(d, "mcp.log"), PLAYBOX_ALLOW_MISSING_LIMITS="1",
               TERMINAL_ENV="playbox", TERMINAL_CWD="/sandbox")
    repo, py = "/sandbox", "/usr/local/venv/bin/python"
    mod = re.search(r"\((.+)\)", r["F"][0]).group(1).rsplit(".", 1)[0]
    hint = f"cd tests && PYTHONPATH={repo} {py} runtests.py --settings=test_sqlite --parallel 1 {mod}"
    prompt = (f"The repository in {repo} has a bug. Fix it in the source code so the behaviour described in the issue is correct, without breaking existing behaviour. "
              f"Use the Python at {py} (all dependencies are installed). Example test command from the repo root: `{hint}`. "
              f"Do not edit existing tests; hidden tests will check your fix. Edit files in place, then reply with a one-line summary.\n\nISSUE:\n{r['problem_statement']}")
    R.kill_my_gateways(hh)
    cmd = [*R.HERMES, "-z", prompt, "-m", "claude-haiku-4-5-20251001", "--provider", "anthropic", "-t", "terminal,file", "--usage-file", usage, "--yolo"]
    t0 = time.time(); note = ""
    try:
        p = subprocess.run(cmd, cwd=d, env=env, capture_output=True, text=True, timeout=TIMEOUT)
        stdout, stderr, rc = p.stdout, p.stderr, p.returncode
    except subprocess.TimeoutExpired as e:
        stdout, stderr, rc, note = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or ""), "", -9, "hermes timeout"
    wall = time.time() - t0
    R.kill_my_gateways(hh)
    open(os.path.join(d, "hermes.out"), "w").write(stdout + "\n--- stderr ---\n" + stderr)
    try: u = json.load(open(usage))
    except Exception: u = {}  # noqa: BLE001
    fin = os.path.join(d, "delegates.final.json")
    fin = json.load(open(fin)) if os.path.exists(fin) else {}
    dl = [json.loads(x) for x in open(os.path.join(d, "delegates.jsonl"))] if os.path.exists(os.path.join(d, "delegates.jsonl")) else []
    ok, aout = host.audit(); open(os.path.join(d, "audit.txt"), "w").write(aout)
    host.close()
    secs = [x["secs"] for x in dl if "secs" in x]
    pb = dict(delegates=len(dl), refused=sum(1 for x in dl if x.get("refused")), edits=sum(1 for x in dl if x.get("edited")),
              rate_waited_s=round(sum(x.get("rate_waited_s", 0) for x in dl), 1), taint_advisories=sum(1 for x in dl if x.get("admission")),
              median_cmd_s=R.pct(secs, .5), total_cmd_s=round(sum(secs), 1), audit_verifies=ok, patch_truncated=bool(fin.get("patch_truncated")),
              refusal_msgs=[x["error"] for x in dl if x.get("refused")][:3], admissions=[x["admission"] for x in dl if x.get("admission")][:2])
    return d, idx, wall, rc, u, fin, pb, note, time.time() - t_setup - wall, "draining" in (stdout + stderr).lower() or not u.get("api_calls")


for attempt in range(3):
    d, idx, wall, rc, u, fin, pb, note, setup_s, failed_early = one_attempt(attempt)
    if not failed_early: break
    print(f"attempt {attempt}: no model call (stale daemon?); retrying", file=sys.stderr)
patch = strip_tests(fin.get("patch") or "")
res = dict(inst=inst, arm="P", n=n, wall_s=round(wall, 1), rc=rc, attempts=attempt + 1, patch_lines=len(patch.splitlines()), usage=u, setup_s=round(setup_s, 1), playbox=pb, note=note)
res.update(score(inst, patch)); res.pop("tail", None); res.pop("ok", None)
json.dump(res, open(f"{out}/{tag}.json", "w"), indent=1); open(f"{out}/{tag}.patch", "w").write(patch)
print(json.dumps({k: v for k, v in res.items() if k not in ("usage", "playbox")}), u.get("estimated_cost_usd"), json.dumps({k: v for k, v in pb.items() if "msgs" not in k and k != "admissions"}))
