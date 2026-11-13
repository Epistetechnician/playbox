"""Held-out-K, harder-question run of the retrieval-then-cache benchmark, on any machine with vLLM (gman node, no Modal).

    VLLM=~/tp/vllm/bin/vllm HF_HOME=~/tp/hf python tools/kv_hybrid_run.py 4b,30b results/kv_hybrid/kv_v3

Phase 1 (dev): a different corpus (seed 100) and different questions pick K for the hybrid: sweep K, keep the K with the best accuracy
(ties: more turns per minute). Phase 2 (test): the original corpus (seed 0) and a second one (seed 1), the chosen K frozen, four
question kinds (lookup, para = the attribute in other words, cross = two projects in one question, drift = the session changes
project half way), arms rag / hybrid / adaptive (hybrid keyed on the question's project, RAG for multi-project questions) and, for
lookup only, the cached full context. 32 agents x 4 turns, prefix cache on, a different cache salt per arm."""
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_steer import kv_hybrid as K

MODELS = {"4b": "Qwen/Qwen3-4B-Instruct-2507", "30b": "Qwen/Qwen3-30B-A3B-Instruct-2507-FP8"}
BASE, S, T, TARGET = "http://127.0.0.1:8000", 32, 4, 128_000
KS = (4, 6, 8, 10, 12, 16, 24)
VLLM = os.environ.get("VLLM", "vllm")


def up():
    try:
        urllib.request.urlopen(BASE + "/health", timeout=3).read()
        return True
    except OSError:
        return False


def serve(path):
    log = open("/tmp/vllm_kv.log", "w")
    p = subprocess.Popen([VLLM, "serve", path, "--served-model-name", "m", "--host", "127.0.0.1", "--port", "8000",
                          "--max-model-len", "147456", "--gpu-memory-utilization", "0.92", "--max-num-seqs", "64",
                          "--enable-prefix-caching", "--enable-prompt-tokens-details"], stdout=log, stderr=subprocess.STDOUT,
                         start_new_session=True)
    t0 = time.time()
    while not up():
        if p.poll() is not None or time.time() - t0 > 1500:
            raise RuntimeError("vllm did not come up:\n" + "\n".join(Path(log.name).read_text(errors="replace").splitlines()[-30:]))
        time.sleep(3)
    return p


def stop(p):
    p.terminate()
    try:
        p.wait(60)
    except subprocess.TimeoutExpired:
        p.kill()
    time.sleep(10)


def corpus(count, seed):
    chunks, facts = K.gen_corpus(TARGET, count, seed=seed)
    return chunks, facts, K.BM25([c["text"] for c in chunks])


def run(chunks, bm, sessions, arm, salt):
    r = K.run_arm(BASE, "m", arm, chunks, bm, sessions, T, salt=salt)
    sm = K.summarize(r)
    sm["wrong"] = [{"project": x["project"], "got": x["text"][:70]} for x in r["rows"] if not x["ok"]][:3]
    return sm


def bench(key):
    from huggingface_hub import snapshot_download
    from transformers import AutoTokenizer
    path = snapshot_download(MODELS[key], allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*", "*.jinja"])
    tok = AutoTokenizer.from_pretrained(path)
    count = lambda s: len(tok(s, add_special_tokens=False).input_ids)
    out = {"model": MODELS[key], "agents": S, "turns": T, "dev": {}, "test": {}}
    p = serve(path)
    try:
        # phase 1: choose K on a different corpus and different questions
        ch, fa, bm = corpus(count, 100)
        dev = K.make_sessions(fa, S, T, seed=11, kind="lookup") + K.make_sessions(fa, S, T, seed=12, kind="para")
        K.stream_chat(BASE, "m", K.build_messages("hybrid8", ch, bm, dev[0], 0, salt="warmup"))
        for k in KS:
            sm = run(ch, bm, dev, f"hybrid{k}", f"{key}-dev-{k}")
            out["dev"][str(k)] = sm
            print(key, "dev K", k, {x: sm[x] for x in ("acc", "turns_per_min", "ttft_later_p50", "prompt_tokens_mean")}, flush=True)
        best = max(KS, key=lambda k: (out["dev"][str(k)]["acc"], out["dev"][str(k)]["turns_per_min"]))
        out["K"] = best
        print(key, "chosen K", best, flush=True)
        # phase 2: frozen K on held-out corpora
        for cs in (0, 1):
            ch, fa, bm = corpus(count, cs)
            K.stream_chat(BASE, "m", K.build_messages("hybrid8", ch, bm, K.make_sessions(fa, 1, 1)[0], 0, salt="warmup"))
            for kind in ("lookup", "para", "cross", "drift"):
                ses = K.make_sessions(fa, S, T, seed=21 + cs, kind=kind)
                arms = ["rag", f"hybrid{best}", f"adaptive{best}"] + (["full"] if kind == "lookup" else [])
                for arm in arms:
                    sm = run(ch, bm, ses, arm, f"{key}-c{cs}-{kind}-{arm}")
                    out["test"][f"c{cs}/{kind}/{arm}"] = sm
                    print(key, f"c{cs}", kind, arm, {x: sm[x] for x in ("acc", "turns_per_min", "ttft_later_p50", "prompt_tokens_mean")}, flush=True)
    finally:
        stop(p)
    return out


if __name__ == "__main__":
    models, outp = sys.argv[1].split(","), Path(sys.argv[2])
    outp.parent.mkdir(parents=True, exist_ok=True)
    for k in models:
        try:
            r = bench(k)
        except Exception as e:
            r = {"model": k, "error": f"{type(e).__name__}: {e}"[:3000]}
        Path(f"{outp}_{k}.json").write_text(json.dumps(r, indent=1))
        print("wrote", f"{outp}_{k}.json", flush=True)
