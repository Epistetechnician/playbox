# Combined Intel TDX + NVIDIA H100 CC (tdx+gpu) Confidential Space image for Parametric AtlasKV.
# Build context: repository root. Requires pre-built Linux x86_64 `playbox` binary at `target/release/playbox`.
FROM nvidia/cuda:12.4.1-runtime-ubuntu22.04

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PLAYBOX_BIN=/usr/local/bin/playbox

RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip curl ca-certificates git && \
    rm -rf /var/lib/apt/lists/* && \
    pip3 install --no-cache-dir \
      torch==2.5.1 transformers==4.46.3 safetensors cryptography huggingface_hub \
      nv-attestation-sdk==2.4.0

COPY target/playbox-linux-amd64 /usr/local/bin/playbox
COPY scripts/gpu-cc-evidence.py /usr/local/bin/gpu-cc-evidence.py
COPY deploy/t3-tdx-gpu-entrypoint.sh /usr/local/bin/t3-tdx-gpu-entrypoint.sh
COPY docs/reports/google-confidential-space-jwks.json /opt/playbox/google-confidential-space-jwks.json
COPY docs/reports/nvidia-nras-jwks.json /opt/playbox/nvidia-nras-jwks.json
COPY hermes-playbox-backend /opt/playbox/hermes-playbox-backend
COPY tools/bench/hermes-parity/run_parametric_atlaskv_eval.py /opt/playbox/run_parametric_atlaskv_eval.py

RUN chmod +x /usr/local/bin/playbox /usr/local/bin/gpu-cc-evidence.py /usr/local/bin/t3-tdx-gpu-entrypoint.sh

LABEL "tee.launch_policy.allow_env_override"="PLAYBOX_CS_AUDIENCE,PLAYBOX_CS_PROJECTS,PLAYBOX_CS_IMAGES,PLAYBOX_GPU_DRIVERS,PLAYBOX_GPU_VBIOS,PLAYBOX_GPU_COUNT,ATLASKV_MODEL,GCS_BUCKET,GCS_OUT_PREFIX"
LABEL "tee.launch_policy.log_redirect"="always"
LABEL "tee.launch_policy.monitoring_memory_allow"="always"

ENTRYPOINT ["/usr/local/bin/t3-tdx-gpu-entrypoint.sh"]
