"""Two Hermes agents, each the 'body' of a different model family. A privately holds a concept, B never sees it.
They share only a vector file produced by `telepathy_agent think` and read by `telepathy_agent listen`.

    python tools/telepathy_hermes.py [max_trials]

Runs 12 randomized trials (5 concepts + 1 no-concept control, in each direction). Hermes is started with --yolo so the
agents can run the one shell command each is given; run it only where that is acceptable."""
import json, os, random, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PY = ROOT.parent / "judge/.venv/bin/python"
OUT = Path(os.environ.get("TELEPATHY_OUT", ROOT / "results/telepathy/hermes"))
CONCEPTS = ["ocean", "space", "music", "sport", "medicine"]
ENV = f"cd {ROOT} && PYTHONPATH=. {PY} -m playbox_steer.telepathy_agent"


PRIVATE = OUT.parent / (OUT.name + "_private")  # A's prompts/outputs and all results: never inside B's working directory
SHARED = OUT.parent / (OUT.name + "_shared")    # B's cwd per trial: holds only the vector file


def hermes(prompt: str, name: str, cwd: Path | None = None) -> str:
    PRIVATE.mkdir(parents=True, exist_ok=True)
    qf = PRIVATE / f"{name}.prompt"
    qf.write_text(prompt)
    flags = []
    if os.environ.get("HERMES_PROVIDER"):  # unset: use the Hermes home's own model (e.g. tools/hermes_local, Ollama)
        flags = ["--provider", os.environ["HERMES_PROVIDER"], "-m", os.environ["HERMES_MODEL"]]
    r = subprocess.run(["hermes", *flags, "-z", prompt, "-t", "terminal"],
                       capture_output=True, text=True, cwd=cwd, timeout=int(os.environ.get("HERMES_TIMEOUT", "300")))
    (PRIVATE / f"{name}.out").write_text(f"exit={r.returncode}\n{r.stdout}\n--- stderr ---\n{r.stderr}")
    return r.stdout


def trial(i: int, src: str, dst: str, concept: str | None) -> dict:
    vec = SHARED / f"t{i}" / "thought.pt"
    vec.parent.mkdir(parents=True, exist_ok=True)
    if concept:
        a = (f"You are Agent A. Your secret topic is: {concept}. Write 6 short, concrete sentences about it, then run exactly this one shell "
             f"command (replace S1..S6 with your sentences, each in quotes):\n"
             f"{ENV} think --src {src} --dst {dst} --out {vec} --text \"S1\" \"S2\" \"S3\" \"S4\" \"S5\" \"S6\"\n"
             f"Do not write the topic word anywhere except inside your own sentences. Reply 'sent' when done.")
    else:
        a = (f"You are Agent A. Run exactly this shell command, with nothing else:\n"
             f"{ENV} think --src {src} --dst {dst} --out {vec} --text \"The item was placed on the table.\" \"It was a normal day.\" "
             f"\"They talked for a while.\"\nReply 'sent' when done.")
    hermes(a, f"A_{i}")
    assert vec.exists(), "A produced no vector"
    b = (f"You are Agent B. Another agent left you a thought as a vector file, not as text. Run this shell command:\n"
         f"{ENV} listen --src {src} --dst {dst} --vec {vec}\n"
         f"It prints continuations written by your own model under the influence of that thought. Read them and decide which ONE of these "
         f"topics the thought was about: {', '.join(CONCEPTS)}. Use only that command's output; do not look at any other files. The final line of your reply must be exactly 'ANSWER: <topic>' "
         f"(or 'ANSWER: none' if you see no topic).")
    out = hermes(b, f"B_{i}", cwd=vec.parent)
    ans = [l for l in out.splitlines() if l.strip().startswith("ANSWER:")]
    guess = ans[-1].split(":", 1)[1].strip().lower().strip(".*`") if ans else None
    return {"i": i, "src": src, "dst": dst, "truth": concept or "none", "guess": guess, "ok": guess == (concept or "none")}


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    plan = []
    for src, dst in (("smollm2", "qwen3"), ("qwen3", "smollm2")):
        plan += [(src, dst, c) for c in CONCEPTS] + [(src, dst, None)]
    random.Random(0).shuffle(plan)
    if len(sys.argv) > 1:
        plan = plan[: int(sys.argv[1])]
    res = []
    for i, (s, d, c) in enumerate(plan):
        t = time.time()
        try:
            r = trial(i, s, d, c)
        except Exception as e:
            r = {"i": i, "src": s, "dst": d, "truth": c or "none", "error": str(e), "ok": False}
        r["secs"] = round(time.time() - t)
        res.append(r)
        print(json.dumps(r), flush=True)
        (PRIVATE / "results.json").write_text(json.dumps(res, indent=1))
