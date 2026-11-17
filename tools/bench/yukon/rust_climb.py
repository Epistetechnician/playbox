#!/usr/bin/env python3
"""Hillclimb the ecdsa.fail Rust track: agents edit src/point_add, a VM scores in parallel.

Laptop: Fable advisor proposes distinct hypotheses; Sonnet workers (Read/Grep/Glob/Edit only,
no shell) edit a private copy of src/point_add. VM: each candidate is built and scored by the
challenge's own ./benchmark.sh in its own checkout. Lower score is better. Nothing is submitted.

Env: CLIMB_DIR, VM (gcloud instance), CLIMB_CAP (USD, default 3), CLIMB_WORKERS, CLIMB_ROUNDS.
"""
import json, os, re, shutil, subprocess, sys, time, concurrent.futures as cf
from pathlib import Path

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


def sh(args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


def ssh(cmd, timeout=1800):
    return sh(["gcloud", "compute", "ssh", VM, f"--zone={ZONE}", f"--project={PROJECT}",
               "--command", cmd], timeout=timeout)


def scp_dir(local, remote):
    return sh(["gcloud", "compute", "scp", "--recurse", f"--zone={ZONE}", f"--project={PROJECT}",
               str(local), f"{VM}:{remote}"], timeout=600)


def claude(prompt, model, cwd, tools, budget, effort="low", allowed=None):
    global spent
    r = sh(["claude", "-p", "--model", model, "--effort", effort, "--tools", tools,
            "--permission-mode", "acceptEdits", "--max-budget-usd", str(budget),
            *(["--allowedTools", allowed] if allowed else []),
            "--output-format", "json", prompt], cwd=cwd, timeout=2700)
    try:
        j = json.loads(r.stdout)
        spent += float(j.get("total_cost_usd", 0))
        return j.get("result", "")
    except Exception:
        return r.stdout[-2000:] + r.stderr[-500:]


def advise(history):
    files = sorted(p.name for p in (BASE / "src/point_add").glob("*.rs"))
    p = (f"Goal: lower the score (Toffoli count x peak qubits, now {BASELINE}: 722696 x 1227) of a reversible "
         f"secp256k1 point-addition circuit. Editable code: src/point_add ({len(files)} modules: {', '.join(files[:60])}). "
         f"Past attempts this session:\n{history or 'none'}\n"
         f"Give {N} DISTINCT, concrete, small, low-risk hypotheses (one module or one parameter each) that could "
         "reduce Toffoli or qubit count while keeping every correctness check. Reply as a JSON list of strings only.")
    out = claude(p, ADVISOR_MODEL, BASE, "Read,Grep,Glob", 0.5)
    m = re.search(r"\[.*\]", out, re.S)
    try:
        hyp = json.loads(m.group(0))
        return [str(h) for h in hyp][:N]
    except Exception:
        return ["Look for a redundant uncompute or a freeable ancilla in the largest module and remove it."] * N


def worker(i, rnd, hypothesis, feedback):
    w = OUT / f"r{rnd}w{i}"
    if w.exists():
        shutil.rmtree(w)
    (w / "src").mkdir(parents=True)
    shutil.copytree(BASE / "src/point_add", w / "src/point_add")
    shutil.copy(BASE / "README.md", w / "README.md")
    ip = sh(["gcloud", "compute", "instances", "describe", VM, f"--zone={ZONE}", f"--project={PROJECT}",
             "--format=value(networkInterfaces[0].accessConfigs[0].natIP)"]).stdout.strip()
    vs = w / "vmscore.sh"
    vs.write_text(f"""#!/bin/bash
# Build and score src/point_add on the VM with the real benchmark (about 6 min). Prints the verdict.
set -u
S="ssh -i $HOME/.ssh/google_compute_engine -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR"
H=shaanp@{ip}; ID=r{rnd}w{i}
$S $H "rm -rf ~/work/cand/$ID && mkdir -p ~/work/cand/$ID/src && rsync -a --exclude target --exclude /src/point_add ~/work/ecdsafail-challenge/ ~/work/cand/$ID/" || exit 2
rsync -a --delete -e "$S" src/point_add/ $H:work/cand/$ID/src/point_add/ || exit 2
$S $H "cd ~/work/cand/$ID && cp -a ~/work/ecdsafail-challenge/target . && timeout 1500 ./benchmark.sh >out.log 2>&1; echo exit=\\$?; grep -E 'avg executed Toffoli|qubits  |all 9024|mismatch|panicked|error|left:|right:' out.log | head -12; python3 -c 'import json;print(json.load(open(\\"score.json\\")))' 2>/dev/null"
""")
    vs.chmod(0o755)
    p = ("You are improving a reversible-circuit builder in src/point_add (Rust). Score = Toffoli x peak qubits, lower "
         f"is better; current best {BASELINE}. A hidden checker requires exact correctness, full uncomputation and zero "
         "phase. Run ./vmscore.sh (the ONLY command you may run; about 6 minutes; at most 3 runs) to build and score your "
         "edit on a real machine, and use its output to fix or refine. Note the code pins some op counts "
         "(source_edits.rs asserts); if an assertion fails, undo or adapt. Score = avg Toffoli x qubits (baseline 722696 x 1227). "
         "Finish with the best state of your edits (revert anything that does not score better) and a one-line result."
         f" Hypothesis to try: {hypothesis}\n" + (f"Feedback from your last attempt: {feedback}\n" if feedback else "")
         + "Edit only files under src/point_add. Finish with a one-line summary of the change.")
    summary = claude(p, WORKER_MODEL, w, "Read,Grep,Glob,Edit,Write,Bash", WBUDGET, effort="medium", allowed="Bash(./vmscore.sh)")
    diff = sh(["diff", "-ru", "-x", ".claude", str(BASE / "src/point_add"), str(w / "src/point_add")]).stdout
    print(f"  r{rnd}w{i}: diff={diff.count(chr(10))} lines; {summary[-200:]!r}", flush=True)
    return {"id": f"r{rnd}w{i}", "dir": w, "hyp": hypothesis, "summary": summary[-400:], "diff_lines": diff.count("\n")}


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
        "echo \"$id $rc $sc\" > RESULT; }; "
        "for id in $(ls ~/work/cand/in); do run $id & done; wait; cat ~/work/cand/*/RESULT")
    r = ssh(script, timeout=3000)
    res = {}
    for line in r.stdout.splitlines():
        t = line.split()
        if len(t) >= 2 and t[0].startswith("r"):
            res[t[0]] = {"exit": int(t[1]), "score": int(t[2]) if len(t) > 2 else None}
    for c in cands:
        c["result"] = res.get(c["id"], {"exit": None, "score": None})
        if c["result"]["score"] is None:
            t = ssh(f"tail -c 600 ~/work/cand/{c['id']}/out.log").stdout
            c["result"]["log"] = t
    return cands


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    log = OUT / "climb.jsonl"
    best, history, feedback = BASELINE, "", {}
    for rnd in range(1, ROUNDS + 1):
        if spent >= CAP:
            print("cap reached", spent); break
        hyps = advise(history)
        print(f"round {rnd}: spent ${spent:.2f}; hypotheses: {hyps}", flush=True)
        with cf.ThreadPoolExecutor(N) as ex:
            cands = list(ex.map(lambda ih: worker(ih[0], rnd, ih[1], feedback.get(ih[0])), enumerate(hyps)))
        cands = [c for c in cands if c["diff_lines"] > 0]
        if not cands:
            history += f"round {rnd}: no edits produced\n"; continue
        score(cands)
        for i, c in enumerate(cands):
            s = c["result"]["score"]
            line = f"{c['id']} score={s} exit={c['result']['exit']} hyp={c['hyp'][:90]!r}"
            print(line, flush=True)
            history += line + "\n"
            feedback[int(c["id"].split("w")[1])] = (f"scored {s}" if s else "failed: " + c["result"].get("log", "")[-400:])
            with open(log, "a") as f:
                f.write(json.dumps({**c, "dir": str(c["dir"]), "spent": spent}) + "\n")
            if s and s < best:
                best = s
                shutil.copytree(c["dir"] / "src/point_add", OUT / "best_point_add", dirs_exist_ok=True)
                print(f"NEW BEST {best} ({c['id']})", flush=True)
    print(f"done. best={best} baseline={BASELINE} spent=${spent:.2f}")


if __name__ == "__main__":
    main()
