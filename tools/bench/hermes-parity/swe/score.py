"""Score an agent's patch on a SWE-bench instance in an arm64 image (network none): apply patch + hidden test_patch, run tests.
resolved = all FAIL_TO_PASS and PASS_TO_PASS pass (SWE-bench's definition). Our own harness, not the official one."""
import json, os, re, subprocess, sys, tempfile
D = os.path.expanduser("~/.playbox-hermes/openshell-parity/swe")
INST = {"flask": ("pallets__flask-4045", "pb-swe-flask:arm64"), "django": ("django__django-12700", "pb-swe-django:arm64")}

def load(name):
    r = json.load(open(f"{D}/{INST[name][0]}.inst.json")); r["F"] = json.loads(r["FAIL_TO_PASS"]); r["P"] = json.loads(r["PASS_TO_PASS"]); return r

def score(name, patch):
    r = load(name); img = INST[name][1]
    with tempfile.TemporaryDirectory() as t:
        open(f"{t}/agent.patch", "w").write(patch); open(f"{t}/test.patch", "w").write(r["test_patch"])
        if name == "flask":
            files = sorted(set(x.split("::")[0] for x in r["F"] + r["P"]))
            run = "pytest -rA -p no:cacheprovider " + " ".join(files)
        else:
            files = sorted(set(re.search(r"\((.+)\)", x).group(1).rsplit(".", 1)[0] for x in r["F"] + r["P"] if "(" in x))
            run = "cd tests && python runtests.py --verbosity 2 --settings=test_sqlite --parallel 1 " + " ".join(files)
        sh = f"cd /usr/local/testbed && git apply /s/agent.patch && git apply /s/test.patch && ({run}) 2>&1"
        if not patch.strip(): sh = sh.replace("git apply /s/agent.patch && ", "")
        p = subprocess.run(["docker", "run", "--rm", "--network", "none", "-v", f"{t}:/s:ro", "--entrypoint", "bash", img, "-c", sh], capture_output=True, text=True, timeout=1500)
    out = p.stdout + p.stderr
    ok = set()
    cur = None
    for ln in out.splitlines():
        if name == "flask":
            m = re.match(r"PASSED (\S+)", ln)
            if m: ok.add(m.group(1))
        else:
            m = re.match(r"(test\w+ \([\w.]+\))", ln)
            if m: cur = m.group(1)
            if ln.endswith("... ok") and cur: ok.add(cur); cur = None
    f = sum(x in ok for x in r["F"])
    cal = f"{D}/{name}.p2p_ok.json"   # P2P tests our parser sees passing under the gold patch (calibrated once; others e.g. skipped are excluded)
    if os.path.exists(cal): r["P"] = json.load(open(cal))
    pp = sum(x in ok for x in r["P"])
    return dict(ok=sorted(ok), f2p=f, f2p_total=len(r["F"]), p2p=pp, p2p_total=len(r["P"]), resolved=(f == len(r["F"]) and pp == len(r["P"])), applied=("error: patch" not in out and "does not apply" not in out), tail=out[-300:])

if __name__ == "__main__":
    name, which = sys.argv[1], sys.argv[2]   # which: none | gold
    patch = load(name)["patch"] if which in ("gold", "calibrate") else ""
    if which == "calibrate":
        os.path.exists(f"{D}/{name}.p2p_ok.json") and os.remove(f"{D}/{name}.p2p_ok.json")
    res = score(name, patch)
    if which == "calibrate":
        P = load(name)["P"]; json.dump([x for x in P if x in set(res["ok"])], open(f"{D}/{name}.p2p_ok.json", "w"))
    res.pop("ok"); print(json.dumps(res, indent=1))
