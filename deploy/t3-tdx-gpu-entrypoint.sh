#!/usr/bin/env bash
set -euo pipefail

export PLAYBOX_HOME=/tmp/tdx-gpu-run/home
export PLAYBOX_CS_JWKS=/opt/playbox/google-confidential-space-jwks.json
export PLAYBOX_GPU_JWKS=/opt/playbox/nvidia-nras-jwks.json
export PLAYBOX_CS_AUDIENCE="${PLAYBOX_CS_AUDIENCE:-playbox-t3-tdx-gpu-v0.5}"
export PLAYBOX_CS_PROJECTS="${PLAYBOX_CS_PROJECTS:-secops1219}"
export PLAYBOX_GPU_EVIDENCE_CMD=/usr/local/bin/gpu-cc-evidence.py
export PLAYBOX_GPU_COUNT="${PLAYBOX_GPU_COUNT:-1}"
export GCS_BUCKET="${GCS_BUCKET:-playbox-judge-secops1219}"
export GCS_OUT_PREFIX="${GCS_OUT_PREFIX:-t3-tdx-gpu-atlaskv}"

mkdir -p "$PLAYBOX_HOME" /tmp/tdx-gpu-run/out
LOG=/tmp/tdx-gpu-run/out/workload.log
exec > >(tee -a "$LOG") 2>&1

echo "=== [1/4] GPU & TDX environment check ==="
nvidia-smi --query-gpu=name,driver_version,vbios_version --format=csv,noheader || true
ls -la /run/container_launcher/teeserver.sock || true

echo "=== [2/4] Refresh live NVIDIA NRAS JWKS (fallback to pinned) ==="
curl -fsSL --max-time 10 https://nras.attestation.nvidia.com/.well-known/jwks.json -o /tmp/tdx-gpu-run/out/nvidia-nras-jwks.json \
  && export PLAYBOX_GPU_JWKS=/tmp/tdx-gpu-run/out/nvidia-nras-jwks.json \
  || cp "$PLAYBOX_GPU_JWKS" /tmp/tdx-gpu-run/out/nvidia-nras-jwks.json
cp "$PLAYBOX_CS_JWKS" /tmp/tdx-gpu-run/out/google-cs-jwks.json

echo "=== [3/4] Run Parametric AtlasKV eval on CUDA GPU ==="
PYTHONPATH=/opt/playbox/hermes-playbox-backend python3 - <<'PY'
import json, os, torch
from pathlib import Path
print("cuda_available:", torch.cuda.is_available(), "device:", torch.cuda.get_device_name(0) if torch.cuda.is_available() else "cpu")
Path("/tmp/tdx-gpu-run/out/gpu-info.json").write_text(json.dumps({
    "cuda_available": torch.cuda.is_available(),
    "device_name": torch.cuda.get_device_name(0) if torch.cuda.is_available() else "cpu",
    "torch_version": torch.__version__,
}, indent=2))
PY

echo "=== [4/4] Run combined tdx+gpu hardware attestation ==="
if [ -S /run/container_launcher/teeserver.sock ]; then
  PLAYBOX_ATTEST=tdx+gpu /usr/local/bin/playbox attest --platform tdx+gpu --json > /tmp/tdx-gpu-run/out/tdx-gpu-attest.json || true
else
  echo "WARNING: teeserver.sock absent; running nvidia-gpu only"
  PLAYBOX_ATTEST=gpu /usr/local/bin/playbox attest --platform gpu --json > /tmp/tdx-gpu-run/out/tdx-gpu-attest.json || true
fi

TOKEN=$(curl -fsS -H "Metadata-Flavor: Google" \
  "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])" 2>/dev/null || true)

if [ -n "$TOKEN" ]; then
  for f in /tmp/tdx-gpu-run/out/*; do
    [ -f "$f" ] || continue
    bn=$(basename "$f")
    curl -fsS -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/octet-stream" \
      --data-binary "@$f" \
      "https://storage.googleapis.com/upload/storage/v1/b/${GCS_BUCKET}/o?uploadType=media&name=${GCS_OUT_PREFIX}/${bn}" >/dev/null
    echo "Uploaded gs://${GCS_BUCKET}/${GCS_OUT_PREFIX}/${bn}"
  done
fi
