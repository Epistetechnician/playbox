"""Two Hermes agents, each the 'body' of a different model family. A privately holds a concept, B never sees it.
They share only a vector file produced by `telepathy_agent think` and read by `telepathy_agent listen`.

    python tools/telepathy_hermes.py [max_trials]

Runs 12 randomized trials (5 concepts + 1 no-concept control, in each direction). Hermes is started with --yolo so the
agents can run the one shell command each is given; run it only where that is acceptable."""
import json, os, random, shutil, sys, time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_steer import telepathy_harness as H

ROOT = Path(__file__).resolve().parents[1]
PY = os.environ.get("TELEPATHY_PY") or ROOT.parent / "judge/.venv/bin/python"
OUT = Path(os.environ.get("TELEPATHY_OUT", ROOT / "results/telepathy/hermes"))
CONCEPTS = os.environ.get("TELEPATHY_CONCEPTS", "ocean,space,music,sport,medicine").split(",")
ENV = (f"cd {ROOT} && PYTHONPATH=. " + (f"TELEPATHY_URL={os.environ['TELEPATHY_URL']} " if os.environ.get("TELEPATHY_URL") else "")
       + f"{PY} -m playbox_steer.telepathy_agent")


PRIVATE = OUT.parent / (OUT.name + "_private")  # A's prompts/outputs and all results: never inside B's working directory
SHARED = OUT.parent / (OUT.name + "_shared")    # B's cwd per trial: holds only the vector file

MIN_FREE_GB = float(os.environ.get("TELEPATHY_MIN_FREE_GB", "6"))
TIMEOUT = int(os.environ.get("HERMES_TIMEOUT", "600"))
CONTROLS = json.loads(Path(os.environ["TELEPATHY_CONTROLS_FILE"]).read_text()) if os.environ.get("TELEPATHY_CONTROLS_FILE") else []
ATTEMPTS = int(os.environ.get("TELEPATHY_ATTEMPTS", "3"))
PORT = int(os.environ.get("TELEPATHY_PORT", "8765"))
URL = os.environ.get("TELEPATHY_URL", f"http://127.0.0.1:{PORT}")
AUTOSTART = os.environ.get("TELEPATHY_AUTOSTART", "1") == "1"
HERMES_BIN = os.environ.get("HERMES_BIN", str(Path.home() / ".local/bin/hermes"))  # the pyenv python puts a different `hermes` first on PATH
REMOTE = os.environ.get("TELEPATHY_REMOTE") == "1"  # use the Modal service (tools/telepathy_modal.py) instead of a local daemon
if REMOTE:
    URL = (os.environ.get("TELEPATHY_URL") or (Path.home() / ".config/playbox/telepathy_url").read_text()).strip()
    ENV = f"cd {ROOT} && PYTHONPATH=. {PY} -m playbox_steer.telepathy_remote"  # urllib only: no torch on the agents' machine
    AUTOSTART = False


def _health(url: str) -> bytes:
    import urllib.request
    try:
        return urllib.request.urlopen(url.rstrip("/") + "/health", timeout=20).read()
    except OSError:
        return b""


def hermes(prompt: str, name: str, cwd: Path | None = None) -> str:
    PRIVATE.mkdir(parents=True, exist_ok=True)
    H.wait_for_memory(MIN_FREE_GB)
    (PRIVATE / f"{name}.prompt").write_text(prompt)
    flags = []
    if os.environ.get("HERMES_PROVIDER"):  # unset: use the Hermes home's own model (e.g. tools/hermes_local, Ollama)
        flags = ["--provider", os.environ["HERMES_PROVIDER"], "-m", os.environ["HERMES_MODEL"]]
    code, out, err = H.run_agent([HERMES_BIN, *flags, "-z", prompt, "-t", "terminal"], cwd, TIMEOUT)
    (PRIVATE / f"{name}.out").write_text(f"exit={code}\n{out}\n--- stderr ---\n{err}")
    if code != 0 and not out.strip():
        raise H.InfraError(f"hermes exited {code}: {err.strip()[-200:]}")
    return out


def attempt(i: int, src: str, dst: str, concept: str | None, k: int) -> dict:
    """One clean pass of a trial: fresh directory, daemon checked, A then B. Raises InfraError if it could not run."""
    if AUTOSTART:
        H.ensure_daemon(URL, PY, ROOT, PORT)
    elif REMOTE:  # a cold GPU container takes a few minutes to answer /health
        deadline = time.time() + 900
        while b"true" not in _health(URL):
            if time.time() > deadline:
                raise H.InfraError(f"telepathy service at {URL} did not answer within 15 minutes")
            time.sleep(10)
    elif not H.daemon_up(URL):
        raise H.InfraError(f"no telepathy daemon at {URL}")
    shared = SHARED / f"t{i}"
    shutil.rmtree(shared, ignore_errors=True)  # never let an earlier attempt's vector pass for this one
    shared.mkdir(parents=True)
    vec = shared / "thought.pt"
    tag = f"{i}" if k == 0 else f"{i}.{k}"
    ctl = None
    if concept and concept.startswith("@"):  # a control from TELEPATHY_CONTROLS_FILE: "@<index>"
        ctl, concept = CONTROLS[int(concept[1:])], None
    if concept:
        a = (f"You are Agent A. Your secret topic is: {concept}. Write 6 short, concrete sentences about it, then run exactly this one shell "
             f"command (replace S1..S6 with your sentences, each in quotes):\n"
             f"{ENV} think --src {src} --dst {dst} --out {vec} --text \"S1\" \"S2\" \"S3\" \"S4\" \"S5\" \"S6\"\n"
             f"Do not write the topic word anywhere except inside your own sentences. Reply 'sent' only after the command printed 'wrote'; "
             f"if it printed an error, run it again once.")
    elif ctl and ctl.get("live"):  # A writes the control text itself, with the same instructions it gets in calibration
        from playbox_steer import telepathy_calib_data as D
        a = D.prompt_for(ctl["live"], ctl["subject"], ENV, src, dst, vec)
    else:
        texts = " ".join(json.dumps(t) for t in (ctl["sentences"] if ctl else
                         ["The item was placed on the table.", "It was a normal day.", "They talked for a while."]))
        a = (f"You are Agent A. Run exactly this shell command, with nothing else:\n"
             f"{ENV} think --src {src} --dst {dst} --out {vec} --text {texts}\nReply 'sent' only after the command printed 'wrote'; if it printed an error, run it again once.")
    hermes(a, f"A_{tag}")
    if not H.vector_ok(vec):
        raise H.InfraError("A produced no vector")
    topics = random.Random(i).sample(CONCEPTS, len(CONCEPTS))
    b = (f"You are Agent B. Another agent left you a thought as a vector file, not as text. Run this shell command:\n"
         f"{ENV} listen --src {src} --dst {dst} --vec {vec}\n"
         f"It prints a signal-strength line (0 = no thought was sent, 1 = a clear thought), then continuations written by your own model under the influence of that thought. If the strength is below 0.3, answer none. Otherwise read them and decide which ONE of these "
         f"topics the thought was about: {', '.join(topics)}. Use only that command's output; do not look at any other files. "
         f"The final line of your reply must be exactly 'ANSWER: <topic>' (or 'ANSWER: none' if you see no topic).")
    out = hermes(b, f"B_{tag}", cwd=shared)
    guess = H.parse_answer(out, CONCEPTS)
    if guess is None:  # no parsable answer is a harness failure, not a wrong answer; a valid wrong answer is never retried
        raise H.InfraError("B gave no valid ANSWER line")
    return {"i": i, "src": src, "dst": dst, "truth": concept or "none", "guess": guess, "ok": guess == (concept or "none")}


def trial(i: int, src: str, dst: str, concept: str | None) -> dict:
    last = None
    for k in range(ATTEMPTS):
        try:
            r = attempt(i, src, dst, concept, k)
            r["attempts"] = k + 1
            return r
        except H.InfraError as e:
            last = e
            print(json.dumps({"i": i, "attempt": k + 1, "infra": str(e)}), flush=True)
            time.sleep(min(30, 5 * (k + 1)))
    raise last


if __name__ == "__main__":
    import concurrent.futures as cf
    import threading
    OUT.mkdir(parents=True, exist_ok=True)
    PRIVATE.mkdir(parents=True, exist_ok=True)
    reps, controls, workers = (int(os.environ.get(k, d)) for k, d in (("TELEPATHY_REPS", 1), ("TELEPATHY_CONTROLS", 1), ("TELEPATHY_WORKERS", 2)))
    plan = []
    for src, dst in (("smollm2", "qwen3"), ("qwen3", "smollm2")):
        plan += [(src, dst, c) for c in CONCEPTS for _ in range(reps)] + ([(src, dst, f"@{j}") for j in range(len(CONTROLS))] if CONTROLS else [(src, dst, None)] * controls)
    random.Random(0).shuffle(plan)
    if len(sys.argv) > 1:
        plan = plan[: int(sys.argv[1])]
    rfile = PRIVATE / "results.json"
    res, lock = [], threading.Lock()
    if os.environ.get("TELEPATHY_RESUME") == "1":  # keep clean trials, redo the rest
        res = [x for x in H.load_results(rfile) if "error" not in x]
    done = {x["i"] for x in res}

    def one(i, s, d, c):
        t = time.time()
        try:
            r = trial(i, s, d, c)
        except H.InfraError as e:
            r = {"i": i, "src": s, "dst": d, "truth": c or "none", "error": str(e), "ok": False}
        r["secs"] = round(time.time() - t)
        with lock:
            res.append(r)
            print(json.dumps(r), flush=True)
            rfile.write_text(json.dumps(sorted(res, key=lambda x: x["i"]), indent=1))

    with cf.ThreadPoolExecutor(workers) as ex:
        list(ex.map(lambda a: one(*a), [(i, *p) for i, p in enumerate(plan) if i not in done]))
    summ = H.summarize(res, len(CONCEPTS))
    (PRIVATE / "summary.json").write_text(json.dumps(summ, indent=1))
    print(json.dumps(summ, indent=1))
