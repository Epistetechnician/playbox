"""Score an agent's patch on a SWE-bench instance in an arm64 image (network none): apply patch + hidden test_patch, run tests.
resolved = all FAIL_TO_PASS and PASS_TO_PASS pass (SWE-bench's definition). Our own harness, not the official one."""
import hashlib, json, os, re, shlex, shutil, subprocess, sys, tempfile
D = os.environ.get("PB_SWE_DIR", os.path.expanduser("~/.playbox-hermes/openshell-parity/swe"))
INST = {"flask": ("pallets__flask-4045", "pb-swe-flask:arm64"), "django": ("django__django-12700", "pb-swe-django:arm64")}

def _swe_dir():
    return os.environ.get("PB_SWE_DIR", D)

def _inst(name):
    swe_d = _swe_dir()
    direct = f"{swe_d}/{name}.inst.json"
    if os.path.exists(direct):
        meta = json.load(open(direct))
        return (name, meta.get("pb_image", "pb-swe-django:arm64"))
    if name in INST:
        return INST[name]
    if name.startswith("django-"):
        return ("django__" + name, "pb-swe-django:arm64")
    for cand in os.listdir(swe_d):
        if cand.endswith(f"__{name}.inst.json"):
            stem = cand[:-len(".inst.json")]
            meta = json.load(open(f"{swe_d}/{cand}"))
            return (stem, meta.get("pb_image", "pb-swe-django:arm64"))
    ta_direct = f"{swe_d}/track_a_150/{name}.inst.json"
    if os.path.exists(ta_direct):
        meta = json.load(open(ta_direct))
        return (f"track_a_150/{name}", meta.get("pb_image", "pb-swe-polyglot:arm64"))
    raise KeyError(f"unknown SWE instance {name!r} in {swe_d}")

def load(name):
    swe_d = _swe_dir()
    r = json.load(open(f"{swe_d}/{_inst(name)[0]}.inst.json"))
    f_raw = r.get("FAIL_TO_PASS", [])
    p_raw = r.get("PASS_TO_PASS", [])
    r["F"] = json.loads(f_raw) if isinstance(f_raw, str) else list(f_raw)
    r["P"] = json.loads(p_raw) if isinstance(p_raw, str) else list(p_raw)
    return r


def _score_official_swebench(r, patch):
    if not (patch or "").strip():
        return {
            "ok": [],
            "f2p": 0,
            "f2p_total": 1,
            "p2p": 0,
            "p2p_total": 0,
            "resolved": False,
            "applied": False,
            "tail": "empty_patch",
        }
    remote_ssh = os.environ.get("PB_REMOTE_GCE_SSH", "").strip()
    remote_gce = os.environ.get("PB_REMOTE_GCE_SCORER", "").strip()
    if remote_ssh or remote_gce:
        req = {
            "instance_id": r["instance_id"],
            "patch": patch or "",
            "expected_sealed_sha": r.get("pb_sealed_holdout_sha256") or "",
            "timeout_s": int(r.get("pb_eval_timeout_s") or 600),
        }
        remote_cmd = "sg docker -c '~/playbox-offload/.venv-swe/bin/python ~/playbox-offload/remote_swe_grade.py'"
        if remote_ssh:
            ssh_key = os.path.expanduser("~/.ssh/google_compute_engine")
            cmd = [
                "ssh", "-o", "StrictHostKeyChecking=no", "-o", "ConnectTimeout=15",
                "-o", "ServerAliveInterval=20", "-o", "ServerAliveCountMax=6",
                "-i", ssh_key, remote_ssh, remote_cmd,
            ]
        else:
            zone = os.environ.get("PB_REMOTE_GCE_ZONE", "us-central1-a").strip()
            cmd = ["gcloud", "compute", "ssh", remote_gce, f"--zone={zone}", "--quiet", "--command", remote_cmd]
        last_err = ""
        for _att in range(3):
            p = subprocess.run(cmd, input=json.dumps(req), capture_output=True, text=True, timeout=1800)
            if p.returncode == 0:
                lines = [ln.strip() for ln in p.stdout.splitlines() if ln.strip().startswith("{")]
                if lines:
                    return json.loads(lines[-1])
            last_err = (p.stderr or p.stdout)[-600:]
        raise RuntimeError(f"remote GCE swebench scorer failed for {r.get('instance_id')}: {last_err}")
    py_bin = r.get("pb_swebench_python") or "/Users/shaanp/Documents/GitHub/patelshaan/primitives/track_a/.venv/bin/python"
    ta_repo = r.get("pb_track_a_repo_root") or "/Users/shaanp/Documents/GitHub/patelshaan"
    sealed_jsonl = r.get("pb_sealed_jsonl") or os.path.join(ta_repo, "primitives/track_a/.sealed/holdout_gold_tests.jsonl")
    expected_sealed_sha = r.get("pb_sealed_holdout_sha256") or ""
    if expected_sealed_sha and os.path.isfile(sealed_jsonl):
        actual_sha = hashlib.sha256(open(sealed_jsonl, "rb").read()).hexdigest()
        if actual_sha != expected_sealed_sha:
            raise RuntimeError(f"Sealed holdout SHA-256 mismatch: {actual_sha} != {expected_sealed_sha}")
    helper = (
        "import fcntl, json, os, shutil, subprocess, sys, time\n"
        "os.environ['HF_DATASETS_OFFLINE'] = '1'\n"
        "os.environ['HF_HUB_OFFLINE'] = '1'\n"
        f"sys.path.insert(0, {ta_repo!r})\n"
        "from pathlib import Path\n"
        "from primitives.track_a.swe_grader import SweGrader\n"
        "req = json.loads(sys.stdin.read())\n"
        "iid = req['instance_id']\n"
        "patch = req['patch']\n"
        "lock_fd = open('/tmp/pb_swe_grader.lock', 'w')\n"
        "fcntl.flock(lock_fd, fcntl.LOCK_EX)\n"
        "img = ''\n"
        "try:\n"
        "    if not os.environ.get('GITHUB_TOKEN'):\n"
        "        _tok = subprocess.run(['gh', 'auth', 'token'], capture_output=True, text=True).stdout.strip()\n"
        "        if _tok:\n"
        "            os.environ['GITHUB_TOKEN'] = _tok\n"
        "    g = SweGrader(split_path=Path(req['split_path']), sealed_jsonl=Path(req['sealed_jsonl']), eval_timeout_s=int(req.get('timeout_s', 600)), only_instance_id=iid)\n"
        "    for _att in range(3):\n"
        "        try:\n"
        "            img = g.ensure_image(iid)\n"
        "            break\n"
        "        except Exception:\n"
        "            if _att == 2:\n"
        "                raise\n"
        "            time.sleep(3 * (_att + 1))\n"
        "    res = g.grade_patch(iid, patch)\n"
        "finally:\n"
        "    keep_img = os.environ.get('PB_KEEP_SWEBENCH_IMAGE') == '1'\n"
        "    if img and not keep_img:\n"
        "        subprocess.run(['docker', 'rmi', '-f', img], capture_output=True, text=True)\n"
        "    fcntl.flock(lock_fd, fcntl.LOCK_UN)\n"
        "    lock_fd.close()\n"
        "spec = g._specs[iid]\n"
        "f_tot = len(json.loads(spec.FAIL_TO_PASS) if isinstance(spec.FAIL_TO_PASS, str) else spec.FAIL_TO_PASS)\n"
        "p_tot = len(json.loads(spec.PASS_TO_PASS) if isinstance(spec.PASS_TO_PASS, str) else spec.PASS_TO_PASS)\n"
        "resolved = bool(res.get('resolved'))\n"
        "out = {\n"
        "    'ok': [],\n"
        "    'f2p': f_tot if resolved else 0,\n"
        "    'f2p_total': f_tot,\n"
        "    'p2p': p_tot if resolved else 0,\n"
        "    'p2p_total': p_tot,\n"
        "    'resolved': resolved,\n"
        "    'applied': bool(res.get('patch_successfully_applied')),\n"
        "    'tail': str(res.get('status') or ''),\n"
        "}\n"
        "print(json.dumps(out))\n"
    )
    payload = {
        "instance_id": r["instance_id"],
        "patch": patch or "",
        "split_path": r.get("pb_split_path") or os.path.join(ta_repo, "primitives/track_a/frozen_split_v1.json"),
        "sealed_jsonl": sealed_jsonl,
        "timeout_s": int(r.get("pb_eval_timeout_s") or 600),
    }
    p = subprocess.run([py_bin, "-c", helper], input=json.dumps(payload), capture_output=True, text=True, timeout=1800)
    if p.returncode != 0:
        raise RuntimeError(f"official swebench scorer failed for {r.get('instance_id')}: {(p.stderr or p.stdout)[-600:]}")
    lines = [ln.strip() for ln in p.stdout.splitlines() if ln.strip().startswith("{")]
    return json.loads(lines[-1])

def extract_django_modules(tests):
    mods = set()
    for x in tests:
        m = re.search(r"\(([\w.]+)\)\s*$", x)
        if m and "." in m.group(1):
            mods.add(m.group(1).rsplit(".", 1)[0])
    return sorted(mods)

def score(name, patch):
    r = load(name)
    if r.get("pb_official_swebench"):
        return _score_official_swebench(r, patch)
    img = _inst(name)[1]
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
