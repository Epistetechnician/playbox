#!/usr/bin/env python3
"""Hermes parity: arm L (Hermes local terminal) vs arm P (Hermes terminal commands delegated through a Playbox room into an OpenShell microVM).
Usage: run.py ARM N [--tag T]   ARM = L | P.  Results: $PB_DIR/results.jsonl (default ~/.playbox-hermes/parity-playbox).
Hard budget: refuses to start once $PB_DIR/budget.count reaches PB_MAX_RUNS (30). Never reads ~/.hermes/.env; each run uses its own HERMES_HOME
(copy of config.yaml, edited only in the copy), so the real config is never touched."""
import json, os, shutil, subprocess, sys, threading, time

H = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(H, "..", "..", ".."))
sys.path.insert(0, os.path.join(REPO, "tools", "bench", "tripwire")); sys.path.insert(0, os.path.join(REPO, "tools"))
import task as T  # noqa: E402
import playbox_watch  # noqa: E402

D = os.environ.get("PB_DIR", os.path.expanduser("~/.playbox-hermes/parity-playbox"))
MAXRUNS = int(os.environ.get("PB_MAX_RUNS", "30"))
TIMEOUT = int(os.environ.get("PB_TIMEOUT", "1500"))
PLAYBOX = os.environ.get("PLAYBOX_BIN", os.path.expanduser("~/.local/bin/playbox"))
# Not the ~/.local/bin/hermes shim: it execs into whichever per-home python last rewrote it (a sibling's deleted home broke it once).
HERMES = [os.path.expanduser("~/.hermes/tools/python-3.14.9+20261021-darwin-arm64/bin/python3"), "-m", "hermes_cli.main"]
HVENV_PY = os.path.expanduser("~/.hermes/hermes-agent/venv/bin/python")
IMAGE = "hermes-agent-nonroot:latest"
HOSTING = os.path.expanduser("~/.playbox/hosting")
PROMPT = ("Fix the bugs in the Python package `mini`. The repo root is {path} (it is a git repo; work only there). "
          "Visible tests: `python -m unittest discover -s tests -v` (run from the repo root). There are also hidden tests: read each function's docstring and make the function "
          "do exactly what it says, including edge cases. Six functions are buggy. Fix all of them, then stop and say what you changed.")
TOOLSETS = os.environ.get("PB_TOOLSETS", "terminal,file")


def budget(inc=True):
    f = os.path.join(D, "budget.count")
    n = int(open(f).read()) if os.path.exists(f) else 0
    if n >= MAXRUNS:
        sys.exit(f"budget exhausted ({n}/{MAXRUNS} Hermes runs)")
    if inc:
        open(f, "w").write(str(n + 1))
    return n + 1


def make_home(hh, arm, plugin_dir=None):
    os.makedirs(hh, exist_ok=True)
    pl = os.path.join(hh, "plugins", "playbox-terminal")
    if os.path.islink(pl): os.remove(pl)
    cfg = subprocess.run([HVENV_PY, "-c", "import yaml,sys,os;c=yaml.safe_load(open(os.path.expanduser('~/.hermes/config.yaml')));"
                          "arm=sys.argv[1];t=c.setdefault('terminal',{});"
                          "p=c.setdefault('plugins',{});e=[x for x in p.get('enabled',[]) if x not in ('playbox','playbox-witness')];p['enabled']=e;"
                          "(c.get('mcp_servers') or {}).pop('playbox',None);"
                          "(t.update(backend='playbox',cwd='/sandbox') or e.append('playbox-terminal')) if arm=='P' else None;"
                          "print(yaml.safe_dump(c))", arm], capture_output=True, text=True, check=True).stdout
    open(os.path.join(hh, "config.yaml"), "w").write(cfg)
    if arm == "P":
        os.makedirs(os.path.join(hh, "plugins"), exist_ok=True)
        os.symlink(plugin_dir, os.path.join(hh, "plugins", "playbox-terminal"))


class Host:
    def __init__(self, d):
        self.d = d
        self.home = os.path.join(d, "phome"); os.makedirs(self.home)
        self.hosting = os.path.join(d, "hosting"); os.makedirs(self.hosting)
        self.env = {**os.environ, "PLAYBOX_HOME": self.home, "PLAYBOX_HOSTING_DIR": self.hosting, "PLAYBOX_ALLOW_MISSING_LIMITS": "1"}
        self.p = subprocess.Popen([PLAYBOX, "up", "--home", self.home, "--local", "--console", "--state", os.path.join(d, "state"), "--repo", os.path.join(d, "repo"),
                                   "--runtime", "openshell", "--allow-image", IMAGE, "--grant-ttl", "43200"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, env=self.env)
        self.links = []
        threading.Thread(target=self._pump, daemon=True).start()
        for _ in range(60):
            if self.links: break
            time.sleep(0.5)
        else:
            raise RuntimeError("playbox host did not come up")
        for _ in range(4):
            self.cmd("link"); time.sleep(1.5)
        self._reg = None

    def _pump(self):
        for ln in self.p.stdout:
            for w in ln.split():
                if w.startswith("playbox://"): self.links.append(w)

    def cmd(self, c):
        self.p.stdin.write(c + "\n"); self.p.stdin.flush()

    def hub_register(self):
        os.makedirs(HOSTING, exist_ok=True)
        for f in os.listdir(self.hosting):
            if f.endswith(".json"):
                shutil.copy(os.path.join(self.hosting, f), os.path.join(HOSTING, f))
                sock = os.path.join(HOSTING, f[:-5] + ".sock")
                try: os.remove(sock)
                except OSError: pass
                os.symlink(os.path.join(self.hosting, f[:-5] + ".sock"), sock)
                self._reg = f[:-5]

    def hub_unregister(self):
        for ext in ("json", "sock"):
            try: os.remove(os.path.join(HOSTING, f"{self._reg}.{ext}")) if self._reg else None
            except OSError: pass

    def audit(self):
        r = subprocess.run([PLAYBOX, "audit", os.path.join(self.d, "state", "audit.log"), "--tail", "5000"], capture_output=True, text=True, env=self.env)
        return r.returncode == 0, r.stdout

    def close(self):
        self.hub_unregister()
        try: self.cmd("quit"); self.p.wait(timeout=15)
        except Exception: self.p.kill()  # noqa: BLE001


def kill_my_gateways(home):
    """`hermes -z` talks to a per-home gateway daemon that outlives the run and keeps its own environment (and plugin code). Stop only the
    daemons whose HERMES_HOME is this arm's home, by exact PID, so every run starts one that sees this run's PLAYBOX_* variables."""
    pids = subprocess.run(["pgrep", "-f", "hermes_cli.main.*gateway"], capture_output=True, text=True).stdout.split()
    for pid in pids:
        env = subprocess.run(["ps", "eww", "-p", pid], capture_output=True, text=True).stdout.split()
        if f"HERMES_HOME={home}" in env:
            try: os.kill(int(pid), 15)
            except Exception: pass  # noqa: BLE001
    time.sleep(2)


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))] if xs else None


def main():
    arm, n = sys.argv[1], sys.argv[2]
    tag = sys.argv[sys.argv.index("--tag") + 1] if "--tag" in sys.argv else "run"
    os.makedirs(D, exist_ok=True)
    idx = budget()
    d = os.path.join(D, f"{tag}-{arm}-{n}")
    shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
    T.make_task(d)                                           # d/repo (git, branch main), d/hidden
    work = os.path.join(d, "work")
    host, rec, note = None, {}, ""
    t0 = time.time()
    try:
        env = {**os.environ, "HERMES_HOME": os.path.join(D, "home-" + arm), "PYTHONUNBUFFERED": "1", "PYTHONPATH": os.path.expanduser("~/.hermes/hermes-agent")}   # one persistent home per arm: a fresh home re-installs its runtime on first use
        for k in ("PLAYBOX_HOME", "PLAYBOX_HOSTING_DIR"): env.pop(k, None)
        usage = os.path.join(d, "usage.json")
        if arm == "L":
            shutil.copytree(os.path.join(d, "repo"), work, symlinks=True)
            make_home(env["HERMES_HOME"], "L")
            path, cwd = work, work
            env["TERMINAL_CWD"] = work
        else:
            host = Host(d)
            host.hub_register()
            make_home(env["HERMES_HOME"], "P", os.path.join(REPO, "hermes-playbox-backend", "playbox_terminal"))
            lf = os.path.join(d, "link.txt"); open(lf, "w").write(host.links[1]); os.chmod(lf, 0o600)
            env.update(PLAYBOX_BIN=PLAYBOX, PLAYBOX_HOME=os.path.join(d, "mhome"), PLAYBOX_LINK_FILE=lf, PLAYBOX_IMAGE=IMAGE,
                       PLAYBOX_STATS=os.path.join(d, "delegates.jsonl"), PLAYBOX_MCP_LOG=os.path.join(d, "mcp.log"), PLAYBOX_ALLOW_MISSING_LIMITS="1", TERMINAL_ENV="playbox", TERMINAL_CWD="/sandbox")
            path, cwd = "/sandbox", d
        kill_my_gateways(env["HERMES_HOME"])
        argv = [*HERMES, "-z", PROMPT.format(path=path), "-t", TOOLSETS, "--usage-file", usage, "--yolo"]
        p = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=TIMEOUT)
        kill_my_gateways(env["HERMES_HOME"])
        open(os.path.join(d, "hermes.out"), "w").write(p.stdout + "\n--- stderr ---\n" + p.stderr)
        blob = (p.stdout + p.stderr).lower()
        if "429" in blob or "usage limit" in blob or "rate limit" in blob:
            note = "RATE/USAGE LIMIT TEXT SEEN"
        try: rec["usage"] = json.load(open(usage))
        except Exception: rec["usage"] = None  # noqa: BLE001
        if arm == "L":
            tree = work
        else:
            tree = os.path.join(d, "solution")
            shutil.copytree(os.path.join(d, "repo"), tree, ignore=shutil.ignore_patterns(".git"))
            subprocess.run(["git", "init", "-q", tree], check=True)
            fin = json.load(open(os.path.join(d, "delegates.final.json"))) if os.path.exists(os.path.join(d, "delegates.final.json")) else {}
            patch = fin.get("patch") or ""
            rec["patch_truncated"] = fin.get("patch_truncated")
            if fin.get("patch_truncated"):
                note += " patch truncated: scoring by replay on host sandbox"
                bench = playbox_watch.Mcp([PLAYBOX, "mcp"], os.path.join(d, "bhome"))
                bench.call("playbox_join", {"link": host.links[2]}); time.sleep(2)
                script = "export PYTHONDONTWRITEBYTECODE=1\n" + "\n".join(f"( cd {c}; {{\n{x}\n}} ) >/tmp/.pbx_replay 2>&1" for c, x in fin["edits"])
                r = bench.call("playbox_delegate", {"base": "main", "command": script, "image": IMAGE, "timeout_secs": 120, "network": False, "apply": True})
                patch = subprocess.run(["git", "-C", os.path.join(d, "repo"), "diff"], capture_output=True, text=True).stdout  # placeholder, replaced below
                ws = [os.path.join(d, "bhome", "workspaces", x) for x in os.listdir(os.path.join(d, "bhome", "workspaces"))][0]
                shutil.rmtree(tree); shutil.copytree(ws, tree, ignore=shutil.ignore_patterns(".git")); bench.close(); patch = ""
            if patch:
                r = subprocess.run(["git", "-C", tree, "apply", "--whitespace=nowarn", "-"], input=patch, text=True, capture_output=True)
                if r.returncode: note += f" git apply failed: {r.stderr[:200]}"
        passed, total = T.score_tree(tree, os.path.join(d, "hidden"))
        rec["hidden_pass"], rec["hidden_total"] = passed, total
        if arm == "P":
            dl = [json.loads(x) for x in open(os.path.join(d, "delegates.jsonl"))] if os.path.exists(os.path.join(d, "delegates.jsonl")) else []
            secs = [x["secs"] for x in dl if "secs" in x]
            ok, aout = host.audit()
            rec["playbox"] = {"delegates": len(dl), "refused": sum(1 for x in dl if x.get("refused")), "edits": sum(1 for x in dl if x.get("edited")),
                              "taint_advisories": sum(1 for x in dl if x.get("admission")), "median_s": pct(secs, .5), "p90_s": pct(secs, .9), "total_cmd_s": round(sum(secs), 1),
                              "audit_verifies": ok, "audit_task_lines": aout.lower().count("task"), "refusal_msgs": [x["error"] for x in dl if x.get("refused")][:3],
                              "admissions": [x["admission"] for x in dl if x.get("admission")][:2]}
            open(os.path.join(d, "audit.txt"), "w").write(aout)
    except subprocess.TimeoutExpired:
        note += " hermes timeout"
    except Exception as e:  # noqa: BLE001
        note += f" driver error: {e!r}"
    finally:
        if host: host.close()
    rec.update(arm=arm, n=n, tag=tag, idx=idx, elapsed_s=round(time.time() - t0), note=note.strip(), at=time.strftime("%H:%M:%S"))
    open(os.path.join(D, "results.jsonl"), "a").write(json.dumps(rec) + "\n")
    print(json.dumps(rec))


if __name__ == "__main__":
    main()
