"""Cached full context vs RAG vs retrieval-then-cache on one 128K corpus, vLLM on an H100 (playbox_steer/kv_hybrid.py).

    modal run --detach steer/tools/kv_hybrid_modal.py [--models 4b,30b]

One container per model. Server 1 has prefix caching on (arms full, rag, hybrid); server 2 has it off (full, rag, hybrid, to
separate what retrieval buys from what the cache buys). 32 concurrent agent sessions x 4 turns, except full without the cache
(1 turn: each request prefills 128K and only about 3 fit in memory at once, so more turns only add minutes). The result is
also saved to the volume: `modal volume get playbox-telepathy-bench /results/kv_<model>.json`. One-shot functions, nothing deployed."""
import json
import os
import subprocess
import time
import urllib.request
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
MODELS = {"4b": "Qwen/Qwen3-4B-Instruct-2507", "30b": "Qwen/Qwen3-30B-A3B-Instruct-2507-FP8"}
app = modal.App("playbox-kv-hybrid")
vol = modal.Volume.from_name("playbox-telepathy-bench", create_if_missing=True)
image = (
    modal.Image.from_registry("nvidia/cuda:12.9.1-devel-ubuntu22.04", add_python="3.12")
    .pip_install("vllm==0.25.1", "huggingface_hub", "transformers")
    .env({"HF_HOME": "/vol/hf", "CUDA_HOME": "/usr/local/cuda", "VLLM_USE_DEEP_GEMM": "0"})
    .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"])
)
BASE = "http://127.0.0.1:8000"
TARGET_TOKENS = 128_000


def _up() -> bool:
    try:
        urllib.request.urlopen(BASE + "/health", timeout=3).read()
        return True
    except OSError:
        return False


def _serve(path: str, cache: bool):
    log = open(f"/tmp/vllm_{int(cache)}.log", "w")
    p = subprocess.Popen(["vllm", "serve", path, "--served-model-name", "m", "--host", "127.0.0.1", "--port", "8000",
                          "--max-model-len", "147456", "--gpu-memory-utilization", "0.92", "--max-num-seqs", "64",
                          "--enable-prefix-caching" if cache else "--no-enable-prefix-caching", "--enable-prompt-tokens-details"],
                         stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    t0 = time.time()
    while not _up():
        if p.poll() is not None or time.time() - t0 > 1200:
            tail = "\n".join(Path(log.name).read_text(errors="replace").splitlines()[-40:])
            raise RuntimeError("vllm did not come up:\n" + tail)
        time.sleep(3)
    return p


def _stop(p):
    p.terminate()
    try:
        p.wait(60)
    except subprocess.TimeoutExpired:
        p.kill()
    time.sleep(10)


def _bench(key: str) -> dict:
    import sys
    sys.path.insert(0, "/root/svc")
    from huggingface_hub import snapshot_download
    from transformers import AutoTokenizer
    from playbox_steer import kv_hybrid as K
    repo = MODELS[key]
    path = snapshot_download(repo, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*", "*.jinja"])
    tok = AutoTokenizer.from_pretrained(path)
    count = lambda s: len(tok(s, add_special_tokens=False).input_ids)
    chunks, facts = K.gen_corpus(TARGET_TOKENS, count)
    bm = K.BM25([c["text"] for c in chunks])
    full_tokens = count(K.build_messages("full", chunks, bm, K.make_sessions(facts, 1, 1)[0], 0)[0]["content"])
    SCEN = {"indep": dict(S=32, P=32, T=4), "team": dict(S=64, P=8, T=16)}  # team: 64 agents on 8 projects, 16 turns each
    sess = {n: K.make_sessions(facts, c["S"], c["T"], n_projects=c["P"]) for n, c in SCEN.items()}
    out = {"model": repo, "gpu": "H100", "vllm": "0.25.1", "scenarios": SCEN, "corpus_chunks": len(chunks),
           "corpus_tokens": full_tokens, "arms": {}, "recall": {}, "prompt_tokens": {}}
    for n, ss in sess.items():  # does the context the arm builds even contain the answer
        for arm, k in (("rag", 5), ("hybrid", 24), ("hybrid10", 10)):
            ok = tot = 0
            for s_ in ss:
                for t in s_["turns"]:
                    idx = bm.top(t["q"], 5) if arm == "rag" else sorted(bm.top(f"Project {s_['project']}", k))
                    ok += t["answer"] in K.context_text([chunks[i] for i in idx]); tot += 1
            out["recall"][f"{n}/{arm}"] = round(ok / tot, 4)
    for arm in ("rag", "hybrid", "hybrid10"):
        out["prompt_tokens"][arm] = count(K.build_messages(arm, chunks, bm, sess["indep"][0], 0)[0]["content"])

    def go(scn, arm, tag, turns=None):
        c = SCEN[scn]
        r = K.run_arm(BASE, "m", arm, chunks, bm, sess[scn], turns or c["T"])
        sm = K.summarize(r)
        sm["wrong"] = [{"project": x["project"], "got": x["text"][:60]} for x in r["rows"] if not x["ok"]][:4]
        if turns: sm["turns_run"] = turns
        out["arms"][f"{scn}/{arm}{tag}"] = sm
        print(key, scn, arm, tag, {k_: sm[k_] for k_ in ("acc", "ttft_turn1_p50", "ttft_later_p50", "turns_per_min", "cached_frac", "prompt_tokens_mean")}, flush=True)

    p = _serve(path, cache=True)
    try:
        warm = K.stream_chat(BASE, "m", K.build_messages("full", chunks, bm, sess["indep"][0], 0))  # first request: nothing cached
        out["cold_full_first_request"] = {"ttft": round(warm["ttft"], 2), "prompt_tokens": warm["prompt_tokens"]}
        for arm in ("full", "rag", "hybrid", "hybrid10"):
            go("indep", arm, "+cache")
        for arm in ("rag", "hybrid", "hybrid10"):
            go("team", arm, "+cache")
    finally:
        _stop(p)
    p = _serve(path, cache=False)
    try:
        go("indep", "full", "-nocache", turns=1)
        for arm in ("rag", "hybrid", "hybrid10"):
            go("indep", arm, "-nocache")
        for arm in ("rag", "hybrid10"):
            go("team", arm, "-nocache")
    finally:
        _stop(p)
    return out


@app.function(image=image, gpu="H100", volumes={"/vol": vol}, timeout=10800)
def bench(key: str) -> dict:
    try:
        r = _bench(key)
    except Exception as e:  # keep the evidence of a failed start
        r = {"model": key, "error": f"{type(e).__name__}: {e}"[:4000]}
    os.makedirs("/vol/results", exist_ok=True)
    Path(f"/vol/results/kv_{key}.json").write_text(json.dumps(r))
    vol.commit()
    return r


@app.local_entrypoint()
def main(models: str = "4b,30b"):
    calls = {k: bench.spawn(k) for k in models.split(",")}
    for k, c in calls.items():
        try:
            r = c.get()
            Path(STEER / f"results/kv_hybrid/kv_{k}.json").write_text(json.dumps(r, indent=1))
            print("wrote", k, r.get("error", "ok")[:200] if isinstance(r.get("error"), str) else "ok")
        except Exception as e:
            print("fetch failed", k, repr(e)[:200], "- result is on the volume")
