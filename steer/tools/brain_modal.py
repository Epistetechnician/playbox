"""An open-weights agent brain on Modal: Qwen3.8-27B-FP8 behind vLLM's OpenAI-compatible API, with tool calling.

    modal deploy steer/tools/brain_modal.py
    modal app stop playbox-brain               # when done; the H100 also scales to zero after 10 idle minutes

Auth: bearer token from the Modal secret `brain-token` (variable BRAIN_TOKEN), passed to vLLM as --api-key.
Hermes points at <url>/v1 through steer/tools/hermes_modal/config.yaml (key read from the environment, not stored).
"""
import subprocess

import modal

MODEL = "Qwen/Qwen3.8-27B-FP8"
app = modal.App("playbox-brain")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)  # shared Hugging Face cache (/vol/hf)
image = (
    # the devel image has nvcc (>= 12.9) for vLLM's JIT-compiled kernels; DeepGEMM is off (not needed at this load)
    modal.Image.from_registry("nvidia/cuda:12.9.1-devel-ubuntu22.04", add_python="3.12")
    .pip_install("vllm==0.33.0", "huggingface_hub")
    .env({"HF_HOME": "/vol/hf", "CUDA_HOME": "/usr/local/cuda", "VLLM_USE_DEEP_GEMM": "0"})
)


@app.function(image=image, gpu="H100", volumes={"/vol": vol}, secrets=[modal.Secret.from_name("brain-token")],
              timeout=3 * 3600, scaledown_window=600, max_containers=1)
@modal.concurrent(max_inputs=32)
@modal.web_server(8000, startup_timeout=1800)
def serve():
    import os
    from huggingface_hub import snapshot_download

    snapshot_download(MODEL)
    vol.commit()
    subprocess.Popen([
        "vllm", "serve", MODEL, "--served-model-name", "qwen3.8-27b", "--host", "0.0.0.0", "--port", "8000",
        "--api-key", os.environ["BRAIN_TOKEN"], "--max-model-len", "131072", "--gpu-memory-utilization", "0.90",
        "--enable-auto-tool-choice", "--tool-call-parser", "qwen3_coder", "--reasoning-parser", "qwen3",
        "--enable-prefix-caching", "--max-num-seqs", "64",
    ])
