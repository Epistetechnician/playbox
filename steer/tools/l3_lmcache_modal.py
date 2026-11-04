"""L3 swarm demo on Modal: one LMCache multiprocess server and four vLLM engines (A writer, B1-B3 readers) on one GPU,
all serving the same model, sharing KV through the server (docs/l3-lmcache.md).

    modal run steer/tools/l3_lmcache_modal.py                 # 24 trials + 8 chain trials, writes results/l3_lmcache/l3.json
    modal run steer/tools/l3_lmcache_modal.py --smoke True    # 2 trials, to check the stack

Pinned to the versions LMCache validated for engine-to-engine sharing (LMCache 0.5.1, vLLM 0.25.1). The function runs and
exits; nothing stays deployed, so there is nothing to stop afterwards (`modal app list` shows no l3 app when it ends).
"""
import json
import subprocess
import time
import urllib.request
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
REPO = "Qwen/Qwen3-1.7B-Base"
NAME = "qwen3-1.7b-base"
LM_PORT, LM_HTTP = 5556, 7000
PORTS = {"A": 8001, "B1": 8002, "B2": 8003, "B3": 8004}

app = modal.App("playbox-l3-lmcache")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)  # Hugging Face cache at /vol/hf
image = (
    modal.Image.from_registry("nvidia/cuda:12.9.1-devel-ubuntu22.04", add_python="3.12")
    .pip_install("vllm==0.25.1", "lmcache==0.5.1", "huggingface_hub")
    .env({"HF_HOME": "/vol/hf", "CUDA_HOME": "/usr/local/cuda", "VLLM_USE_DEEP_GEMM": "0"})
    .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"])
)


def _up(url: str) -> bool:
    try:
        urllib.request.urlopen(url, timeout=3).read()
        return True
    except OSError:
        return False


def _tail(path: str, n: int = 40) -> str:
    try:
        return "\n".join(Path(path).read_text(errors="replace").splitlines()[-n:])
    except OSError:
        return ""


@app.function(image=image, gpu="L40S", memory=98304, volumes={"/vol": vol}, timeout=3600)
def demo(n_trials: int = 24, n_chain: int = 8) -> dict:
    import os
    import sys
    sys.path.insert(0, "/root/svc")
    from huggingface_hub import snapshot_download
    path = snapshot_download(REPO, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"])
    cfg = json.loads((Path(path) / "config.json").read_text())
    kvb = 2 * cfg["num_hidden_layers"] * cfg["num_key_value_heads"] * cfg.get("head_dim", cfg["hidden_size"] // cfg["num_attention_heads"]) * 2

    procs, logs = [], {}

    def start(name, argv, env=None):
        logs[name] = f"/tmp/{name}.log"
        procs.append((name, subprocess.Popen(argv, stdout=open(logs[name], "w"), stderr=subprocess.STDOUT,
                                              env={**os.environ, **(env or {})}, start_new_session=True)))

    def fail(msg):
        out = {"error": msg, "logs": {k: _tail(v) for k, v in logs.items()}}
        return out

    try:
        start("lmcache", ["lmcache", "server", "--host", "127.0.0.1", "--port", str(LM_PORT), "--http-port", str(LM_HTTP),
                          "--chunk-size", "256", "--l1-size-gb", "48", "--eviction-policy", "LRU"])
        t0 = time.time()
        while not _up(f"http://127.0.0.1:{LM_HTTP}/metrics"):
            if time.time() - t0 > 180 or procs[-1][1].poll() is not None:
                return fail("lmcache server did not come up")
            time.sleep(2)
        kvcfg = json.dumps({"kv_connector": "LMCacheMPConnector",
                            "kv_connector_module_path": "lmcache.integration.vllm.lmcache_mp_connector", "kv_role": "kv_both",
                            "kv_connector_extra_config": {"lmcache.mp.host": "tcp://127.0.0.1", "lmcache.mp.port": LM_PORT}})
        for name, port in PORTS.items():  # one at a time: concurrent startup would race the GPU memory profiling
            start(name, ["vllm", "serve", path, "--served-model-name", NAME, "--host", "127.0.0.1", "--port", str(port),
                         "--tensor-parallel-size", "1", "--block-size", "16", "--max-model-len", "32768",
                         "--gpu-memory-utilization", "0.2", "--no-enable-prefix-caching", "--enforce-eager",
                         "--kv-transfer-config", kvcfg])
            t0 = time.time()
            while not _up(f"http://127.0.0.1:{port}/health"):
                if time.time() - t0 > 600 or procs[-1][1].poll() is not None:
                    return fail(f"engine {name} did not come up")
                time.sleep(3)
        from playbox_steer import l3_swarm as S
        res = S.run(PORTS, f"http://127.0.0.1:{LM_HTTP}/metrics", n_trials=n_trials, n_chain=n_chain, kv_bytes_per_token=kvb)
        res["lmcache_metric_names"] = sorted({l.split("{")[0].split(" ")[0] for l in
                                              urllib.request.urlopen(f"http://127.0.0.1:{LM_HTTP}/metrics").read().decode().splitlines()
                                              if l.startswith("lmcache_mp_")})
        res["versions"] = {"lmcache": "0.5.1", "vllm": "0.25.1", "model": REPO, "gpu": "L40S"}
        return res
    except Exception as e:  # return the logs instead of raising, so a failed start is diagnosable from one run
        return fail(f"{type(e).__name__}: {e}")
    finally:
        for _, p in procs:
            try:
                os.killpg(p.pid, 15)
            except (ProcessLookupError, PermissionError):
                pass


@app.local_entrypoint()
def main(smoke: bool = False):
    r = demo.remote(2 if smoke else 24, 1 if smoke else 8)
    out = STEER / "results/l3_lmcache"
    out.mkdir(parents=True, exist_ok=True)
    f = out / ("l3_smoke.json" if smoke else "l3.json")
    f.write_text(json.dumps(r, indent=1))
    if "error" in r:
        print("FAILED:", r["error"])
        for k, v in r["logs"].items():
            print(f"--- {k} ---\n{v}")
    else:
        print(json.dumps(r["summary"], indent=1))
    print("wrote", f)
