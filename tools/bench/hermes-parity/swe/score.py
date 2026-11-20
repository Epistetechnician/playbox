"""Score an agent's patch on a SWE-bench instance in an arm64 image (network none): apply patch + hidden test_patch, run tests.
resolved = all FAIL_TO_PASS and PASS_TO_PASS pass (SWE-bench's definition). Our own harness, not the official one."""
import json, os, re, shlex, shutil, subprocess, sys, tempfile
D = os.environ.get("PB_SWE_DIR", os.path.expanduser("~/.playbox-hermes/openshell-parity/swe"))
INST = {"flask": ("pallets__flask-4045", "pb-swe-flask:arm64"), "django": ("django__django-12700", "pb-swe-django:arm64")}

def _inst(name):
    if name in INST:
        return INST[name]
    if name.startswith("django-"):
        return ("django__" + name, "pb-swe-django:arm64")
    direct = f"{D}/{name}.inst.json"
    if os.path.exists(direct):
        meta = json.load(open(direct))
        return (name, meta.get("pb_image", "pb-swe-django:arm64"))
    for cand in os.listdir(D):
        if cand.endswith(f"__{name}.inst.json"):
            stem = cand[:-len(".inst.json")]
            meta = json.load(open(f"{D}/{cand}"))
            return (stem, meta.get("pb_image", "pb-swe-django:arm64"))
    raise KeyError(f"unknown SWE instance {name!r} in {D}")

def load(name):
    r = json.load(open(f"{D}/{_inst(name)[0]}.inst.json"))
    r["F"] = json.loads(r["FAIL_TO_PASS"]) if isinstance(r["FAIL_TO_PASS"], str) else list(r["FAIL_TO_PASS"])
    r["P"] = json.loads(r["PASS_TO_PASS"]) if isinstance(r["PASS_TO_PASS"], str) else list(r["PASS_TO_PASS"])
    return r

def extract_django_modules(tests):
    mods = set()
    for x in tests:
        m = re.search(r"\(([\w.]+)\)\s*$", x)
        if m and "." in m.group(1):
            mods.add(m.group(1).rsplit(".", 1)[0])
    return sorted(mods)

def score(name, patch):
    r = load(name); img = _inst(name)[1]
    parser = r.get("pb_parser") or ("pytest" if name == "flask" else "django")
    with tempfile.TemporaryDirectory() as t:
        open(f"{t}/agent.patch", "w").write(patch or ""); open(f"{t}/test.patch", "w").write(r["test_patch"])
        if r.get("pb_test_cmd"):
            run = r["pb_test_cmd"]
        elif parser == "pytest":
            files = sorted(set(x.split("::")[0] for x in r["F"] + r["P"] if x))
            py_path = r.get("pb_pythonpath", "/tmp/tb/src:/tmp/tb")
            py_bin = r.get("pb_python", "pytest")
            run = f"PYTHONPATH={py_path} {py_bin} -rA -p no:cacheprovider " + " ".join(shlex.quote(f) for f in files)
        else:
            files = extract_django_modules(r["F"] + r["P"])
            run = "cd tests && PYTHONPATH=/tmp/tb python runtests.py --verbosity 2 --settings=test_sqlite --parallel 1 " + " ".join(shlex.quote(f) for f in files)
        tb_src = r.get("pb_container_testbed", "/usr/local/testbed")
        if r.get("pb_host_tar"):
            shutil.copy(r["pb_host_tar"], f"{t}/repo.tar")
            prep = "mkdir -p /tmp/tb && tar -xf /s/repo.tar -C /tmp/tb && cd /tmp/tb"
        else:
            prep = f"cp -a {tb_src} /tmp/tb && cd /tmp/tb && git checkout -q -f {r['base_commit']}"
        apply_agent = "git apply /s/agent.patch && " if (patch or "").strip() else ""
        apply_test = "git apply /s/test.patch && " if (r.get("test_patch") or "").strip() else ""
        sh = f"{prep} && {apply_agent}{apply_test}({run}) 2>&1"
        p = subprocess.run(["docker", "run", "--rm", "--network", "none", "-v", f"{t}:/s:ro", "--entrypoint", "bash", img, "-c", sh], capture_output=True, text=True, timeout=1500)
    out = p.stdout + p.stderr
    ok = set()
    cur = None
    ansi_re = re.compile(r"\x1b\[[0-9;]*m")
    for raw_ln in out.splitlines():
        ln = ansi_re.sub("", raw_ln)
        if parser == "pytest":
            m = re.match(r"PASSED (\S+)", ln.strip())
            if m: ok.add(m.group(1))
        elif parser == "cargo":
            m = re.match(r"test (\S+) \.\.\. ok\s*$", ln.strip())
            if m: ok.add(m.group(1))
        elif parser == "node":
            m = re.match(r"ok \d+ - (.+?)\s*$", ln.strip())
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
    import shutil
    name, which = sys.argv[1], sys.argv[2]   # which: none | gold | calibrate
    patch = load(name)["patch"] if which in ("gold", "calibrate") else ""
    if which == "calibrate":
        os.path.exists(f"{D}/{name}.p2p_ok.json") and os.remove(f"{D}/{name}.p2p_ok.json")
    res = score(name, patch)
    if which == "calibrate":
        P = load(name)["P"]; json.dump([x for x in P if x in set(res["ok"])], open(f"{D}/{name}.p2p_ok.json", "w"))
        res["p2p_total"] = sum(1 for x in P if x in set(res["ok"]))
        res["p2p"] = res["p2p_total"]
        res["resolved"] = (res["f2p"] == res["f2p_total"])
    res.pop("ok"); print(json.dumps(res, indent=1))
