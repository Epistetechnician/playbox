"""Retrieval-then-cache on public data (QuALITY) and on much larger working sets, vLLM on a gman node.

    VLLM=~/tp/vllm/bin/vllm HF_HOME=~/tp/hf python tools/kv_hybrid_public_run.py 4b,30b results/kv_hybrid/kv_pub

Per model, one server (prefix cache on), every arm with its own cache salt:
  dev   QuALITY train articles (corpus seed 100): hybrid K in 6, 12, 24, 48; K = best accuracy (ties: turns per minute)
  test  QuALITY validation articles, two corpora (seeds 0, 1), K frozen: arms full, rag5, rag<K>, hybrid<K>, doc (the session's own article)
  big   synthetic, 10 projects x 64 chunks (about 13K tokens each), 64 agents x 8 turns: arms full, rag5, hybrid64
See playbox_steer/kv_hybrid_big.py for the workloads."""
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_steer import kv_hybrid as K
from playbox_steer import kv_hybrid_big as B

MODELS = {"4b": "Qwen/Qwen3-4B-Instruct-2507", "30b": "Qwen/Qwen3-30B-A3B-Instruct-2507-FP8"}
BASE, TARGET = "http://127.0.0.1:8000", 120_000
VLLM = os.environ.get("VLLM", "vllm")
KS = (6, 12, 24, 48)


def up():
    try:
        urllib.request.urlopen(BASE + "/health", timeout=3).read()
        return True
    except OSError:
        return False


def serve(path):
    log = open("/tmp/vllm_pub.log", "w")
    p = subprocess.Popen([VLLM, "serve", path, "--served-model-name", "m", "--host", "127.0.0.1", "--port", "8000",
                          "--max-model-len", "147456", "--gpu-memory-utilization", "0.92", "--max-num-seqs", "64",
                          "--enable-prefix-caching", "--enable-prompt-tokens-details"], stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
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


def load_quality(split):
    try:
        import pyarrow.parquet as pq
    except ImportError:
        subprocess.check_call([sys.executable, "-m", "pip", "install", "-q", "pyarrow"])
        import pyarrow.parquet as pq
    from huggingface_hub import hf_hub_download, list_repo_files
    f = next(x for x in list_repo_files("emozilla/quality", repo_type="dataset") if x.startswith(f"data/{split}-"))
    t = pq.read_table(hf_hub_download("emozilla/quality", f, repo_type="dataset")).to_pylist()
    return B.group_articles(t)


def go(chunks, bm, sessions, arm, salt, turns, builder, **kw):
    r = K.run_arm(BASE, "m", arm, chunks, bm, sessions, turns, builder=builder, salt=salt, **kw)
    sm = K.summarize(r)
    sm["sessions"], sm["turns"] = len(sessions), turns
    return sm


def bench(key):
    from huggingface_hub import snapshot_download
    from transformers import AutoTokenizer
    path = snapshot_download(MODELS[key], allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*", "*.jinja"])
    tok = AutoTokenizer.from_pretrained(path)
    count = lambda s: len(tok(s, add_special_tokens=False).input_ids)
    out = {"model": MODELS[key], "dev": {}, "test": {}, "big": {}}
    train, val = load_quality("train"), load_quality("validation")
    out["quality_articles"] = {"train": len(train), "validation": len(val)}
    p = serve(path)
    try:
        ch, ss = B.build_quality(train, TARGET, count, seed=100)
        bm = K.BM25([c["text"] for c in ch])
        out["dev_corpus"] = {"articles": len(ss), "chunks": len(ch), "questions": sum(len(s["turns"]) for s in ss)}
        K.stream_chat(BASE, "m", B.quality_messages("hybrid6", ch, bm, ss[0], 0, salt="warmup"))
        for k in KS:
            sm = go(ch, bm, ss, f"hybrid{k}", f"{key}-dev-{k}", 8, B.quality_messages)
            out["dev"][str(k)] = sm
            print(key, "dev K", k, {x: sm[x] for x in ("acc", "turns_per_min", "ttft_later_p50", "prompt_tokens_mean")}, flush=True)
        best = max(KS, key=lambda k: (out["dev"][str(k)]["acc"], out["dev"][str(k)]["turns_per_min"]))
        out["K"] = best
        print(key, "chosen K", best, flush=True)
        for cs in (0, 1):
            ch, ss = B.build_quality(val, TARGET, count, seed=cs)
            bm = K.BM25([c["text"] for c in ch])
            out.setdefault("test_corpus", {})[str(cs)] = {"articles": len(ss), "chunks": len(ch), "questions": sum(len(s["turns"]) for s in ss),
                                                          "full_prompt_tokens": count(B.quality_messages("full", ch, bm, ss[0], 0)[0]["content"])}
            for arm in ("full", "rag5", f"rag{best}", f"hybrid{best}", "doc"):
                sm = go(ch, bm, ss, arm, f"{key}-q{cs}-{arm}", 8, B.quality_messages)
                out["test"][f"q{cs}/{arm}"] = sm
                print(key, f"q{cs}", arm, {x: sm[x] for x in ("acc", "n", "turns_per_min", "ttft_later_p50", "prompt_tokens_mean")}, flush=True)
        ch, facts = B.gen_corpus_big(TARGET, count, seed=0)
        bm = K.BM25([c["text"] for c in ch])
        ss = B.make_sessions_big(facts, 64, 8, n_projects=10)
        out["big_corpus"] = {"chunks": len(ch), "projects": len({c["project"] for c in ch}),
                             "hybrid64_prompt_tokens": count(K.build_messages("hybrid64", ch, bm, ss[0], 0)[0]["content"])}
        for arm in ("full", "rag", "hybrid64"):
            sm = go(ch, bm, ss, arm, f"{key}-big-{arm}", 8, None)
            out["big"][arm] = sm
            print(key, "big", arm, {x: sm[x] for x in ("acc", "n", "turns_per_min", "ttft_later_p50", "prompt_tokens_mean")}, flush=True)
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
