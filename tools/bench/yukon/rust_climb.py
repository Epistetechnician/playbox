#!/usr/bin/env python3
"""Hillclimb the ecdsa.fail Rust track: agents edit src/point_add, a VM scores in parallel.

Laptop: Fable advisor proposes distinct hypotheses; Sonnet workers (Read/Grep/Glob/Edit only,
no shell) edit a private copy of src/point_add. VM: each candidate is built and scored by the
challenge's own ./benchmark.sh in its own checkout. Lower score is better. Nothing is submitted.

Env: CLIMB_DIR, VM (gcloud instance), CLIMB_CAP (USD, default 3), CLIMB_WORKERS, CLIMB_ROUNDS.
"""
import concurrent.futures as cf
import contextlib
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from sim import Sim

HOME = Path.home()
BASE = Path(os.environ.get("CLIMB_SRC", HOME / "yukon-work/ecdsafail-challenge"))
OUT = Path(os.environ.get("CLIMB_DIR", HOME / ".playbox-rust"))
VM = os.environ.get("VM", "yukon-rnd-1")
ZONE, PROJECT = "us-central1-a", "secops1219"
CAP = float(os.environ.get("CLIMB_CAP", "3"))
N = int(os.environ.get("CLIMB_WORKERS", "4"))
ROUNDS = int(os.environ.get("CLIMB_ROUNDS", "2"))
WORKER_MODEL = os.environ.get("CLIMB_MODEL", "claude-sonnet-5-5")
ADVISOR_MODEL = os.environ.get("CLIMB_ADVISOR", "claude-sonnet-5-5")  # Fable needs usage credits
BASELINE = 886747992
WBUDGET = float(os.environ.get("CLIMB_WBUDGET", "0.3"))
spent = 0.0
SPENT_LOCK = threading.Lock()


def sh(args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


def ssh(cmd, timeout=1800):
    try:
        return sh(["gcloud", "compute", "ssh", VM, f"--zone={ZONE}", f"--project={PROJECT}",
                   "--command", cmd], timeout=timeout)
    except subprocess.TimeoutExpired:  # a hung ssh poll must not kill the climb
        return subprocess.CompletedProcess(cmd, 255, "", "ssh timed out")


def scp_dir(local, remote):
    return sh(["gcloud", "compute", "scp", "--recurse", f"--zone={ZONE}", f"--project={PROJECT}",
               str(local), f"{VM}:{remote}"], timeout=600)


CLI = os.environ.get("CLIMB_CLI", "claude")  # "agy": Antigravity CLI (Gemini 4); edit-only, no shell, no USD budget cap


def agy(prompt, model, cwd):
    prompt = ("You have NO shell: never run commands. Use only the file read, edit and search tools. "
              "A denied command ends your turn with nothing done.\n\n" + prompt)
    r = sh(["agy", "-p", prompt, "--model", model, "--output-format", "json", "--print-timeout", "1500s",
            "--settings", json.dumps({"permissions": {"allow": ["write_file(*)", "edit_file(*)", "read_file(*)"]}})],
           cwd=cwd, timeout=2700)
    try:
        return json.loads(r.stdout.splitlines()[-1]).get("response", "")
    except Exception:
        return r.stdout[-2000:] + r.stderr[-500:]


def claude(prompt, model, cwd, tools, budget, effort="low", allowed=None):
    global spent
    if CLI == "agy":
        return agy(prompt, model, cwd)
    r = sh(["claude", "-p", "--model", model, "--effort", effort, "--tools", tools,
            "--permission-mode", "acceptEdits", "--max-budget-usd", str(budget),
            *(["--allowedTools", allowed] if allowed else []),
            "--output-format", "json", prompt], cwd=cwd, timeout=2700)
    try:
        j = json.loads(r.stdout)
        with SPENT_LOCK:
            spent += float(j.get("total_cost_usd", 0))
        return j.get("result", "")
    except Exception:
        return r.stdout[-2000:] + r.stderr[-500:]


def _finite_pos(val):
    if isinstance(val, bool) or not isinstance(val, (int, float)):
        return None
    if not math.isfinite(val) or val <= 0:
        return None
    return val


def parse_score_value(raw):
    """Parse a positive finite integer score from evaluator output, or return None."""
    if raw is None or isinstance(raw, bool):
        return None
    if isinstance(raw, (int, float)):
        v = _finite_pos(raw)
        return int(round(v)) if v is not None else None
    s = str(raw).strip()
    if not s:
        return None
    try:
        v = float(s)
    except ValueError:
        return None
    if not math.isfinite(v) or v <= 0:
        return None
    return int(round(v))


def parse_result_lines(stdout):
    """Parse `<id> <exit> [score]` lines from VM RESULT output without raising on malformed lines."""
    res = {}
    for line in (stdout or "").splitlines():
        t = line.split()
        if len(t) < 2 or not t[0].startswith("r"):
            continue
        try:
            rc_code = int(t[1])
        except ValueError:
            rc_code = None
        sc = parse_score_value(t[2]) if len(t) > 2 else None
        res[t[0]] = {"exit": rc_code, "score": sc}
    return res


@contextlib.contextmanager
def climb_lock(out_dir):
    """Acquire a PID lockfile in out_dir, reclaiming stale locks from dead processes."""
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    lock_path = out_dir / ".climb.lock"
    for _ in range(2):
        try:
            fd = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
            os.write(fd, f"{os.getpid()}\n".encode())
            os.close(fd)
            break
        except FileExistsError:
            stale = False
            try:
                pid = int(lock_path.read_text().strip())
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    stale = True
                except PermissionError:
                    stale = False
                else:
                    raise RuntimeError(f"climb already running in {out_dir} (pid {pid})")
            except (ValueError, OSError):
                stale = True
            if stale:
                lock_path.unlink(missing_ok=True)
                continue
            raise
    else:
        raise RuntimeError(f"could not acquire lock {lock_path}")
    try:
        yield lock_path
    finally:
        lock_path.unlink(missing_ok=True)


def save_best_dir(src_dir, dst_dir):
    """Atomically replace dst_dir with a fresh copy of src_dir (no stale files)."""
    src_dir, dst_dir = Path(src_dir), Path(dst_dir)
    tmp_dir = dst_dir.with_name(dst_dir.name + ".tmp")
    shutil.rmtree(tmp_dir, ignore_errors=True)
    shutil.copytree(src_dir, tmp_dir)
    if dst_dir.exists():
        shutil.rmtree(dst_dir)
    tmp_dir.rename(dst_dir)


def advise(history, sim):
    files = sorted(p.name for p in (BASE / "src/point_add").glob("*.rs"))
    p = (f"Goal: lower the score (Toffoli count x peak qubits, now {BASELINE}: 722696 x 1227) of a reversible "
         f"secp256k1 point-addition circuit. Editable code: src/point_add ({len(files)} modules: {', '.join(files[:60])}). "
         f"Self-improvement memory (every past attempt, grouped by root cause, with predicted vs observed score; "
         f"build on what worked, do not repeat what failed or mispredicted):\n{sim.render()}\n"
         f"This session so far:\n{history or 'none'}\n"
         f"Give {N} DISTINCT, concrete, small, low-risk hypotheses (one module or one parameter each) that could "
         "reduce Toffoli or qubit count while keeping every correctness check. Reply as a JSON list of objects "
         '{"root_cause": short recurring-problem label (reuse an existing one if it applies), "hypothesis": the edit, '
         '"predicted_score": integer score you expect}. JSON only.')
    out = claude(p, ADVISOR_MODEL, BASE, "Read,Grep,Glob", 0.5)
    m = re.search(r"\[.*\]", out, re.S)
    try:
        parsed = []
        for h in json.loads(m.group(0))[:N]:
            if isinstance(h, dict):
                parsed.append({"root_cause": str(h.get("root_cause", "unspecified")),
                               "hypothesis": str(h.get("hypothesis", "")),
                               "predicted": parse_score_value(h.get("predicted_score"))})
            else:
                parsed.append({"root_cause": "unspecified", "hypothesis": str(h), "predicted": None})
        if parsed:
            return parsed
    except Exception as exc:
        print(f"advise: falling back to default hypotheses ({exc})", file=sys.stderr)
    return [{"root_cause": "redundant uncompute", "predicted": None, "hypothesis":
             "Look for a redundant uncompute or a freeable ancilla in the largest module and remove it."}] * N


def worker(i, rnd, hyp, feedback):
    hypothesis, root_cause, predicted = hyp["hypothesis"], hyp["root_cause"], hyp["predicted"]
    w = OUT / f"r{rnd}w{i}"
    if w.exists():
        shutil.rmtree(w)
    (w / "src").mkdir(parents=True)
    shutil.copytree(BASE / "src/point_add", w / "src/point_add")
    shutil.copy(BASE / "README.md", w / "README.md")
    ip = sh(["gcloud", "compute", "instances", "describe", VM, f"--zone={ZONE}", f"--project={PROJECT}",
             "--format=value(networkInterfaces[0].accessConfigs[0].natIP)"]).stdout.strip()
    sdir = OUT / "scripts"
    sdir.mkdir(parents=True, exist_ok=True)
    vs = sdir / f"vmscore_r{rnd}w{i}.sh"
    vs_body = f"""#!/bin/bash
# Build and score src/point_add on the VM with the real benchmark (about 6 min). Prints the verdict.
set -u
S="ssh -i $HOME/.ssh/google_compute_engine -o StrictHostKeyChecking=accept-new -o LogLevel=ERROR"
H=shaanp@{ip}; ID=r{rnd}w{i}
$S $H "rm -rf ~/work/cand/$ID && mkdir -p ~/work/cand/$ID/src && rsync -a --exclude target --exclude /src/point_add ~/work/ecdsafail-challenge/ ~/work/cand/$ID/" || exit 2
rsync -a --delete -e "$S" src/point_add/ $H:work/cand/$ID/src/point_add/ || exit 2
$S $H "cd ~/work/cand/$ID && cp -a ~/work/ecdsafail-challenge/target . && timeout 1500 ./benchmark.sh >out.log 2>&1; echo exit=\\$?; grep -E 'avg executed Toffoli|qubits  |all 9024|mismatch|panicked|error|left:|right:' out.log | head -12; python3 -c 'import json;print(json.load(open(\\"score.json\\")))' 2>/dev/null"
"""
    vs.write_text(vs_body)
    vs.chmod(0o555)
    vs_hash = hashlib.sha256(vs_body.encode()).hexdigest()
    p = ("You are improving a reversible-circuit builder in src/point_add (Rust). Score = Toffoli x peak qubits, lower "
         f"is better; current best {BASELINE}. A hidden checker requires exact correctness, full uncomputation and zero "
         f"phase. Run {vs} (the ONLY command you may run; about 6 minutes; at most 3 runs) to build and score your "
         "edit on a real machine, and use its output to fix or refine. Note the code pins some op counts "
         "(source_edits.rs asserts); if an assertion fails, undo or adapt. Score = avg Toffoli x qubits (baseline 722696 x 1227). "
         "Finish with the best state of your edits (revert anything that does not score better) and a one-line result."
         f" Hypothesis to try: {hypothesis}\n" + (f"Feedback from your last attempt: {feedback}\n" if feedback else "")
         + "Edit only files under src/point_add. Finish with a one-line summary of the change.")
    if CLI == "agy":
        p += "\nYou cannot run commands here. Do not run vmscore.sh; just make your best edit. It is built and scored for you afterwards."
    summary = claude(p, WORKER_MODEL, w, "Read,Grep,Glob,Edit,Write,Bash", WBUDGET, effort="medium", allowed=f"Bash({vs})")
    if not vs.exists() or hashlib.sha256(vs.read_bytes()).hexdigest() != vs_hash:
        raise RuntimeError(f"worker {rnd}w{i} tampered with {vs}")
    diff = sh(["diff", "-ru", "-x", ".claude", str(BASE / "src/point_add"), str(w / "src/point_add")]).stdout
    print(f"  r{rnd}w{i}: diff={diff.count(chr(10))} lines; {summary[-200:]!r}", flush=True)
    return {"id": f"r{rnd}w{i}", "dir": w, "hyp": hypothesis, "root_cause": root_cause, "predicted": predicted, "summary": summary[-400:], "diff_lines": diff.count("\n")}


def score(cands):
    """Copy each candidate to the VM and run the real benchmark, up to 4 in parallel."""
    stage = OUT / "stage"
    shutil.rmtree(stage, ignore_errors=True)
    for c in cands:
        shutil.copytree(c["dir"] / "src/point_add", stage / c["id"])
    ssh("rm -rf ~/work/cand && mkdir -p ~/work/cand")
    scp_dir(stage, "~/work/cand/in")
    script = (
        "cd ~/work; run() { id=$1; d=$HOME/work/cand/$id; "
        "mkdir -p $d/src; rsync -a --exclude target --exclude /src/point_add ~/work/ecdsafail-challenge/ $d/; "
        "cp -r $HOME/work/cand/in/$id $d/src/point_add; cp -a ~/work/ecdsafail-challenge/target $d/target; "
        "cd $d; timeout 1500 ./benchmark.sh > out.log 2>&1; rc=$?; "
        "sc=$(python3 -c 'import json;print(json.load(open(\"score.json\"))[\"score\"])' 2>/dev/null); "
        "[ -z \"$sc\" ] && [ $rc = 0 ] && sc=$(python3 -c 'import re;t=open(\"out.log\").read();print(round(float(re.search(r\"avg executed Toffoli\\s*:\\s*([0-9.]+)\",t)[1]))*int(re.search(r\"qubits\\s*:\\s*([0-9]+)\",t)[1]))' 2>/dev/null); "
        "echo \"$id $rc $sc\" > RESULT; }; "
        "for id in $(ls ~/work/cand/in); do run $id & done; wait")
    # detached on the VM so a dropped ssh session cannot kill the benchmarks; poll for the RESULT files
    ssh("cat > ~/work/cand/score.sh <<'EOS'\n" + script + "\nEOS\nrm -f ~/work/cand/*/RESULT ~/work/cand/DONE; "
        "setsid nohup bash -c 'bash ~/work/cand/score.sh; touch ~/work/cand/DONE' >~/work/cand/score.log 2>&1 < /dev/null & echo started")
    t0 = time.time()
    while time.time() - t0 < 3000:
        time.sleep(30)
        p = ssh("test -f ~/work/cand/DONE && cat ~/work/cand/*/RESULT; echo \"poll=$?\"", timeout=120)
        if p.returncode == 0 and "poll=0" in p.stdout:
            break
        if p.returncode:
            print(f"  poll ssh exited {p.returncode}, retrying", flush=True)
    r = p
    res = parse_result_lines(r.stdout)
    for c in cands:
        c["result"] = res.get(c["id"], {"exit": None, "score": None})
        r = c["result"]
        if r["score"] is None:
            r["reason"] = ("benchmark timed out (1500s)" if r["exit"] == 124 else
                           f"benchmark exited {r['exit']} without a valid score" if r["exit"] is not None else
                           "no RESULT file: ssh dropped, VM stopped, or still running")
            t = ssh(f"cd ~/work/cand/{c['id']} 2>/dev/null && stat -c 'out.log modified %y' out.log; tail -c 600 out.log").stdout
            r["log"] = t
    return cands


def _recover_state(sim):
    """Recover best score and next round index from persisted Sim history."""
    observed = [v for a in sim.attempts if (v := parse_score_value(a.get("observed"))) is not None]
    best = min([BASELINE, *observed])
    rounds = [r for a in sim.attempts if isinstance((r := a.get("prov", {}).get("round")), int)]
    start_round = max(rounds, default=0) + 1
    return best, start_round


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    log = OUT / "climb.jsonl"
    with climb_lock(OUT):
        sim = Sim(OUT / "sim.jsonl")
        best, start_round = _recover_state(sim)
        history, feedback = "", {}
        for rnd in range(start_round, start_round + ROUNDS):
            if spent >= CAP:
                print("cap reached", spent)
                break
            hyps = advise(history, sim)
            print(f"round {rnd}: spent ${spent:.2f}; hypotheses: {[h['hypothesis'] for h in hyps]}", flush=True)
            cands = []
            with cf.ThreadPoolExecutor(N) as ex:
                futs = {ex.submit(worker, i, rnd, h, feedback.get(i)): i for i, h in enumerate(hyps)}
                for fut in cf.as_completed(futs):
                    i = futs[fut]
                    try:
                        cands.append((i, fut.result()))
                    except Exception as exc:
                        print(f"  r{rnd}w{i}: worker failed: {exc}", flush=True)
                        feedback[i] = f"worker crashed: {exc}"
            cands = [c for _, c in sorted(cands) if c["diff_lines"] > 0]
            if not cands:
                history += f"round {rnd}: no edits produced\n"
                continue
            score(cands)
            round_best = best
            for c in cands:
                s = parse_score_value(c["result"].get("score"))
                c["result"]["score"] = s
                rationale = c["summary"] + (f" [score failed: {c['result'].get('reason')}]" if s is None else "")
                a = sim.record(c["root_cause"], c["hyp"], rationale, c["predicted"], s, round_best,
                               model=WORKER_MODEL, round=rnd, run=c["id"])
                line = f"{c['id']} [{a['verdict']}] score={s} exit={c['result']['exit']} hyp={c['hyp'][:90]!r}"
                print(line, flush=True)
                history += line + "\n"
                widx = int(c["id"].split("w")[1])
                feedback[widx] = (f"scored {s}" if s is not None else
                                  f"failed ({c['result'].get('reason')}): " + c["result"].get("log", "")[-400:])
                with open(log, "a") as f:
                    f.write(json.dumps({**c, "dir": str(c["dir"]), "spent": spent}) + "\n")
                if s is not None and s < best:
                    best = s
                    save_best_dir(c["dir"] / "src/point_add", OUT / "best_point_add")
                    print(f"NEW BEST {best} ({c['id']})", flush=True)
        print(f"done. best={best} baseline={BASELINE} spent=${spent:.2f}")


if __name__ == "__main__":
    main()
