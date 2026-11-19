"""The merge gate: evaluates a submission, writes a signed receipt, advances the frontier on a verified win.

    gate.py b CANDIDATE.py --author NAME           # Tier B: lossless speedup on the LM problem
    gate.py a SCHEME.json  --author NAME [--problem tier_a_matmul/problem.json]   # Tier A: exact checker
    gate.py credit | verify | status

Nothing here needs a person in the loop. Tier B evaluates on sealed prompt seeds that rotate after every promotion,
measures the frontier and the candidate interleaved in fresh processes, and requires a second independent seed
("replica") to agree. The replica stands in for a second host; on a real room it runs on another attested machine.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import random
import resource
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from fractions import Fraction
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import ROOT, STATE, append_receipt, sha256_bytes, sha256_file, verify_log  # noqa: E402

B = ROOT / "tier_b_lm"
A = ROOT / "tier_a_matmul"
PB = json.load(open(B / "problem.json"))
LOG = STATE / "receipts.jsonl"
FRONT = STATE / "frontier.json"


def scrubbed_env(home_dir: Path | None = None) -> dict[str, str]:
    # Scrubbed minimal environment: never leak host secrets/tokens or host $HOME to untrusted candidate subprocesses.
    # Pinned thread count roughly halves measured run-to-run spread vs the default.
    home = str(home_dir.resolve()) if home_dir is not None else "/nonexistent"
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": home,
        "TMPDIR": home,
        "HF_HUB_OFFLINE": "1",
        "TOKENIZERS_PARALLELISM": "false",
        "OMP_NUM_THREADS": "4",
        "MKL_NUM_THREADS": "4",
        "VECLIB_MAXIMUM_THREADS": "4",
    }


def _sb_quote(s: str) -> str:
    return json.dumps(s)


def _seatbelt_profile(cand: Path, mdir: Path, tmp_home: Path) -> str:
    exe_root = Path(sys.executable).resolve().parent.parent
    read_paths = []
    for p in (
        Path(sys.prefix).resolve(),
        Path(sys.base_prefix).resolve(),
        exe_root,
        B.resolve(),
        (ROOT / "docs").resolve(),
        mdir.resolve(),
    ):
        s = str(p)
        if s not in read_paths:
            read_paths.append(s)
    subpaths = " ".join(f"(subpath {_sb_quote(p)})" for p in read_paths)
    cand_resolved = str(cand.resolve())
    tmp_resolved = str(tmp_home.resolve())
    state_resolved = str(STATE.resolve())
    return "\n".join(
        [
            "(version 1)",
            "(deny default)",
            "(allow process-exec process-fork signal sysctl-read mach-lookup ipc-posix-shm* file-map-executable)",
            "(deny network*)",
            "(allow file-read-metadata)",
            "(allow file-read*",
            '    (literal "/")',
            '    (subpath "/usr")',
            '    (subpath "/bin")',
            '    (subpath "/sbin")',
            '    (subpath "/System")',
            '    (subpath "/Library")',
            '    (subpath "/opt")',
            '    (subpath "/private/var/db")',
            '    (subpath "/dev")',
            f"    {subpaths}",
            f"    (literal {_sb_quote(cand_resolved)})",
            f"    (subpath {_sb_quote(tmp_resolved)}))",
            f"(deny file-read* (subpath {_sb_quote(state_resolved)}))",
            "(allow file-read* file-write*",
            f"    (subpath {_sb_quote(tmp_resolved)})",
            '    (literal "/dev/null")',
            '    (literal "/dev/urandom")',
            '    (literal "/dev/zero"))',
        ]
    )


def _bwrap_cmd(bench_cmd: list[str], cand: Path, mdir: Path, tmp_home: Path) -> list[str]:
    cmd = [
        "bwrap",
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
    ]
    real_home = Path.home().resolve()
    if real_home.exists():
        cmd.extend(["--tmpfs", str(real_home)])
    if STATE.exists():
        cmd.extend(["--tmpfs", str(STATE.resolve())])
    for p in {
        Path(sys.prefix).resolve(),
        Path(sys.base_prefix).resolve(),
        B.resolve(),
        (ROOT / "docs").resolve(),
        mdir.resolve(),
        cand.resolve(),
    }:
        if p.exists():
            cmd.extend(["--ro-bind", str(p), str(p)])
    cmd.extend(["--dir", str(tmp_home), "--bind", str(tmp_home), str(tmp_home), "--"])
    cmd.extend(bench_cmd)
    return cmd


def _docker_cmd(cand: Path, mdir: Path, seed: int, image: str) -> list[str]:
    cand_r = cand.resolve()
    return [
        "docker",
        "run",
        "--rm",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pids-limit=64",
        "--memory=4g",
        "--cpus=4",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=256m",
        "-e",
        "HOME=/tmp",
        "-e",
        "TMPDIR=/tmp",
        "-e",
        "HF_HUB_OFFLINE=1",
        "-e",
        "TOKENIZERS_PARALLELISM=false",
        "-e",
        "OMP_NUM_THREADS=4",
        "-e",
        "MKL_NUM_THREADS=4",
        "-e",
        "VECLIB_MAXIMUM_THREADS=4",
        "-v",
        f"{B.resolve()}:/repo/challenge/tier_b_lm:ro",
        "-v",
        f"{(ROOT / 'docs').resolve()}:/repo/docs:ro",
        "-v",
        f"{cand_r}:/cand/{cand_r.name}:ro",
        "-v",
        f"{mdir.resolve()}:/model:ro",
        image,
        "python3",
        "/repo/challenge/tier_b_lm/bench.py",
        "--candidate",
        f"/cand/{cand_r.name}",
        "--model",
        "/model",
        "--seed",
        str(seed),
    ]


def _apply_rlimits() -> None:
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    fsize = 64 * 1024 * 1024
    _, hard_f = resource.getrlimit(resource.RLIMIT_FSIZE)
    if hard_f == resource.RLIM_INFINITY or hard_f >= fsize:
        resource.setrlimit(resource.RLIMIT_FSIZE, (fsize, fsize))
    soft_n, hard_n = resource.getrlimit(resource.RLIMIT_NOFILE)
    cap_n = 512
    if hard_n == resource.RLIM_INFINITY or hard_n >= cap_n:
        resource.setrlimit(resource.RLIMIT_NOFILE, (min(soft_n, cap_n), cap_n))


def build_bench_invocation(cand: Path, mdir: Path, seed: int, tmp_home: Path) -> tuple[list[str], dict[str, str], bool]:
    cand_r = cand.resolve()
    mdir_r = mdir.resolve()
    bench_cmd = [sys.executable, str((B / "bench.py").resolve()), "--candidate", str(cand_r), "--model", str(mdir_r), "--seed", str(seed)]
    mode = os.environ.get("PLAYBOX_GATE_SANDBOX", "auto").strip().lower()
    allow_unsafe = os.environ.get("PLAYBOX_ALLOW_UNSAFE_GATE", "").strip().lower() in ("1", "on", "true")
    image = os.environ.get("PLAYBOX_GATE_IMAGE", "").strip()

    if mode == "docker" or (mode == "auto" and image):
        if not image or not shutil.which("docker"):
            raise RuntimeError("PLAYBOX_GATE_SANDBOX=docker requires PLAYBOX_GATE_IMAGE and docker on PATH")
        return _docker_cmd(cand_r, mdir_r, seed, image), {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}, False
    if mode == "bwrap" or (mode == "auto" and sys.platform.startswith("linux") and shutil.which("bwrap")):
        if not shutil.which("bwrap"):
            raise RuntimeError("PLAYBOX_GATE_SANDBOX=bwrap requires bubblewrap (`bwrap`) on PATH")
        return _bwrap_cmd(bench_cmd, cand_r, mdir_r, tmp_home), scrubbed_env(tmp_home), True
    if mode == "sandbox-exec" or (mode == "auto" and sys.platform == "darwin" and Path("/usr/bin/sandbox-exec").exists()):
        if not Path("/usr/bin/sandbox-exec").exists():
            raise RuntimeError("PLAYBOX_GATE_SANDBOX=sandbox-exec requires /usr/bin/sandbox-exec")
        prof = _seatbelt_profile(cand_r, mdir_r, tmp_home)
        return ["/usr/bin/sandbox-exec", "-p", prof, *bench_cmd], scrubbed_env(tmp_home), True
    if allow_unsafe and mode in ("auto", "unsafe"):
        return bench_cmd, scrubbed_env(tmp_home), True
    raise RuntimeError(
        "refusing unsandboxed candidate execution in challenge/gate.py "
        "(configure PLAYBOX_GATE_IMAGE with docker, install bwrap on Linux, use macOS sandbox-exec, "
        "or opt in explicitly with PLAYBOX_ALLOW_UNSAFE_GATE=1)"
    )


def model_dir() -> Path:
    snaps = sorted((Path.home() / ".cache/huggingface/hub/models--HuggingFaceTB--SmolLM2-135M/snapshots").glob("*"))
    return snaps[-1]


def env_hash() -> str:
    import torch
    import transformers
    return sha256_bytes(f"{torch.__version__}|{transformers.__version__}|{sha256_file(model_dir() / 'model.safetensors')}".encode())


def load_front():
    if FRONT.exists():
        f = json.loads(FRONT.read_text())
        if isinstance(f.get("a"), list):
            f["a"] = {"matmul2": f["a"]}
        return f
    return {"b": [{"id": "baseline", "author": "pinned", "parent": None, "path": str(B / "baseline/candidate.py"), "cum": 1.0, "ratio": 1.0}],
            "a": {"matmul2": [{"id": "naive", "author": "pinned", "parent": None, "rank": 8}]}}


def _tier_a_chain(f: dict, prob: dict) -> list[dict]:
    if isinstance(f.get("a"), list):
        f["a"] = {"matmul2": f["a"]}
    chains = f.setdefault("a", {})
    if prob["id"] not in chains:
        chains[prob["id"]] = [{"id": "naive", "author": "pinned", "parent": None, "rank": prob["baseline"]}]
    return chains[prob["id"]]


def save_front(f):
    STATE.mkdir(exist_ok=True)
    FRONT.write_text(json.dumps(f, indent=1))


def run_bench(cand, seed, ref=None, write_ref=None):
    # Never pass --ref into the untrusted candidate subprocess; reference comparison happens here in the parent.
    mdir = model_dir()
    with tempfile.TemporaryDirectory(prefix="pbx-gate-") as td:
        tmp_home = Path(td).resolve()
        cmd, env, use_rlimits = build_bench_invocation(Path(cand), mdir, int(seed), tmp_home)
        t0 = time.perf_counter()
        p = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
            cwd=str(tmp_home),
            start_new_session=True,
            preexec_fn=_apply_rlimits if use_rlimits else None,
        )
        out_chunks: list[str] = []
        err_lines: list[str] = []
        t_start = t_end = None
        timed_out = False

        def _kill():
            nonlocal timed_out
            timed_out = True
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except ProcessLookupError:
                p.kill()

        timer = threading.Timer(900, _kill)
        timer.start()
        out_thread = threading.Thread(target=lambda: out_chunks.append(p.stdout.read()), daemon=True)
        out_thread.start()
        try:
            for line in p.stderr:
                s = line.strip()
                if s == "BENCH_START" and t_start is None:
                    t_start = time.perf_counter()
                elif s == "BENCH_END" and t_end is None:
                    t_end = time.perf_counter()
                else:
                    err_lines.append(line)
            out_thread.join(timeout=10)
            p.wait(timeout=10)
        finally:
            timer.cancel()
            if p.poll() is None:
                try:
                    os.killpg(p.pid, signal.SIGKILL)
                except ProcessLookupError:
                    p.kill()
                p.wait()
    wall = time.perf_counter() - t0
    if timed_out:
        return {"verdict": "fail", "reason": "timeout"}
    stdout = "".join(out_chunks).strip()
    stderr = "".join(err_lines).strip()
    try:
        r = json.loads(stdout.splitlines()[-1])
    except (ValueError, IndexError):
        return {"verdict": "fail", "reason": "harness error: " + stderr[-300:]}
    if r.get("verdict") != "pass":
        return r
    sec = r.get("seconds")
    gen_wall = (t_end - t_start) if (t_start is not None and t_end is not None) else None
    if gen_wall is None or not isinstance(sec, (int, float)) or not math.isfinite(sec) or sec <= 0 or sec > wall + 0.05 or (gen_wall > 0.2 and sec < gen_wall * 0.5):
        return {"verdict": "fail", "reason": "child timing failed parent wall-clock verification"}
    eff_sec = max(float(sec), max(0.0, gen_wall - 0.02))
    outs = r.get("outputs")
    n_tok = PB["new_tokens"]
    shape_ok = isinstance(outs, list) and len(outs) == PB["prompts"] and all(isinstance(o, list) and len(o) == n_tok and all(type(x) is int for x in o) for o in outs)
    if not shape_ok:
        return {"verdict": "fail", "reason": "bad output shape"}
    if write_ref:
        Path(write_ref).write_text(json.dumps(outs))
    if ref:
        expected = json.loads(Path(ref).read_text())
        if len(outs) != len(expected):
            return {"verdict": "fail", "reason": "bad output shape"}
        mism = sum(1 for o, e in zip(outs, expected) if o != e)
        if mism:
            return {"verdict": "fail", "mismatched_prompts": mism, "reason": f"{mism}/{len(expected)} prompts differ from reference"}
    return {"verdict": "pass", "tok_s": PB["prompts"] * n_tok / eff_sec, "seconds": eff_sec}


def epoch():
    ep = STATE / "epoch.json"
    STATE.mkdir(exist_ok=True)
    sr = random.SystemRandom()
    e = json.loads(ep.read_text()) if ep.exists() else {}
    changed = False
    for role in ("primary", "replica"):
        if role not in e:
            e[role] = sr.randrange(1, 2**31)
            changed = True
    if "salt" not in e:
        e["salt"] = sr.randbytes(16).hex()
        changed = True
    if changed:
        fd = os.open(ep, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        try:
            os.write(fd, json.dumps(e).encode())
        finally:
            os.close(fd)
    return e


def ref_for(seed):
    p = STATE / f"ref_{seed}.json"
    if not p.exists():
        print("  computing reference outputs for sealed seed (baseline run)…", flush=True)
        r = run_bench(B / "baseline/candidate.py", seed, write_ref=p)
        assert r.get("verdict") == "pass", r
    return p


def submit_b(path, author):
    f = load_front()
    front = f["b"][-1]
    cand = Path(path).resolve()
    cand_hash = sha256_file(cand)
    e = epoch()
    salt = e["salt"]
    body = {"tier": "b", "problem": PB["id"], "track": PB["track"], "problem_hash": sha256_file(B / "problem.json"),
            "env_hash": env_hash(), "candidate": cand_hash, "author": author, "parent": front["id"],
            "seed_commit": {role: sha256_bytes(f"{salt}:{e[role]}".encode()) for role in ("primary", "replica")}}
    ratios, runs = {}, {}
    sr = random.SystemRandom()
    for role in ("primary", "replica"):
        seed = e[role]
        ref = ref_for(seed)
        cs, fs = [], []
        sr = random.SystemRandom()
        for rep in range(PB["reps"]):
            order = [("cand", cand), ("front", front["path"])]
            sr.shuffle(order)  # randomized order each rep
            for name, pth in order:
                r = run_bench(pth, seed, ref=ref)
                if r.get("verdict") != "pass":
                    who = role if name == "cand" else f"{role} (frontier)"
                    body.update(verdict="reject", reason=f"{who}: {r.get('reason')}", measurements=runs)
                    return finish(body, f)
                if name == "cand":
                    cs.append(r["tok_s"])
                else:
                    fs.append(r["tok_s"])
        if any(ft <= 0 for ft in fs) or any(ct <= 0 for ct in cs):
            body.update(verdict="reject", reason=f"{role}: non-positive throughput", measurements=runs)
            return finish(body, f)
        # Paired ratio: each rep's candidate/frontier runs are adjacent, so load that hits both cancels.
        ratios[role] = statistics.median(ct / ft for ct, ft in zip(cs, fs))
        runs[role] = {"cand_tok_s": cs, "front_tok_s": fs, "ratio": round(ratios[role], 3)}
    worst = min(ratios.values())
    body["measurements"] = runs
    if worst >= PB["threshold"]:
        body.update(verdict="promote", ratio=worst)
        pid = cand_hash[:10]
        dest = STATE / "frontier"
        dest.mkdir(exist_ok=True)
        shutil.copy(cand, dest / f"{pid}.py")
        f["b"].append({"id": pid, "author": author, "parent": front["id"], "path": str(dest / f"{pid}.py"), "ratio": worst, "cum": front["cum"] * worst})
        (STATE / "epoch.json").unlink(missing_ok=True)  # seeds rotate on every promotion
    else:
        body.update(verdict="reject", reason=f"gain {worst:.3f}x over frontier is below threshold {PB['threshold']}x")
    return finish(body, f)


def check2_matmul(n, scheme):
    """Independent second checker: random exact rational matrices, compare scheme output to the true product."""
    sys.path.insert(0, str(A))
    import check as c1

    r = random.Random(1)
    d = n * n
    for _ in range(8):
        a = [Fraction(r.randint(-9, 9), r.randint(1, 5)) for _ in range(d)]
        b = [Fraction(r.randint(-9, 9), r.randint(1, 5)) for _ in range(d)]
        c = [sum(a[i * n + j] * b[j * n + k] for j in range(n)) for i in range(n) for k in range(n)]
        m = [sum(c1.parse_rational(x) * y for x, y in zip(t["u"], a)) * sum(c1.parse_rational(x) * y for x, y in zip(t["v"], b)) for t in scheme["products"]]
        got = [sum(c1.parse_rational(t["w"][o]) * mk for t, mk in zip(scheme["products"], m)) for o in range(d)]
        if got != c:
            return False
    return True


def submit_a(path, author, problem):
    sys.path.insert(0, str(A))
    import check as c1
    prob = json.loads(Path(problem).read_text())
    f = load_front()
    chain = _tier_a_chain(f, prob)
    front = chain[-1]
    body = {"tier": "a", "problem": prob["id"], "problem_hash": sha256_file(Path(problem)), "candidate": sha256_file(Path(path)),
            "author": author, "parent": front["id"]}
    try:
        scheme = json.loads(Path(path).read_text())
    except (ValueError, UnicodeDecodeError) as e:
        body.update(verdict="reject", reason=f"invalid JSON: {e}")
        return finish(body, f)
    r1 = c1.check(prob, scheme)
    if r1["verdict"] != "pass":
        body.update(verdict="reject", reason="checker 1: " + r1.get("reason", ""))
    elif not check2_matmul(prob["n"], scheme):
        body.update(verdict="reject", reason="checker 2 disagrees with checker 1")
    elif r1["score"] >= front["rank"]:
        body.update(verdict="reject", reason=f"rank {r1['score']} does not improve on frontier rank {front['rank']}")
    elif r1["score"] < prob.get("literature", 0) and prob["id"] == "matmul2":
        body.update(verdict="reject", reason="below a proven lower bound: treat as a checker bug and escalate")
    else:
        body.update(verdict="promote", rank=r1["score"])
        pid = sha256_file(Path(path))[:10]
        chain.append({"id": pid, "author": author, "parent": front["id"], "rank": r1["score"]})
    return finish(body, f)


def finish(body, f):
    rec = append_receipt(LOG, body)
    save_front(f)
    print(f"{body['verdict'].upper():8} {body.get('reason') or ''} receipt={rec['id'][:12]}")
    return body


def credit():
    f = load_front()
    shares = {}
    for e in f["b"][1:]:
        shares.setdefault(e["author"], {"b_log_gain": 0.0, "a_rank_drop": 0})["b_log_gain"] += math.log(e["ratio"])
    a_chains = {"matmul2": f["a"]} if isinstance(f["a"], list) else f["a"]
    for chain in a_chains.values():
        prev = None
        for e in chain:
            if prev is not None:
                shares.setdefault(e["author"], {"b_log_gain": 0.0, "a_rank_drop": 0})["a_rank_drop"] += prev - e["rank"]
            prev = e["rank"]
    print(json.dumps(shares, indent=1))
    ranks = {k: c[-1]["rank"] for k, c in a_chains.items()}
    rank_out = ranks["matmul2"] if list(ranks) == ["matmul2"] else ranks
    print("Tier B cumulative speedup over pinned baseline:", round(f["b"][-1]["cum"], 2), "x;  Tier A rank:", rank_out)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for k in ("a", "b"):
        s = sub.add_parser(k)
        s.add_argument("path")
        s.add_argument("--author", required=True)
        if k == "a":
            s.add_argument("--problem", default=str(A / "problem.json"))
    for k in ("credit", "verify", "status"):
        sub.add_parser(k)
    a = ap.parse_args()
    if a.cmd == "b":
        submit_b(a.path, a.author)
    elif a.cmd == "a":
        submit_a(a.path, a.author, a.problem)
    elif a.cmd == "credit":
        credit()
    elif a.cmd == "verify":
        print("receipts verified:", verify_log(LOG))
    else:
        print(json.dumps(load_front(), indent=1))
