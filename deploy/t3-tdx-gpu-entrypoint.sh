#!/usr/bin/env bash
set -euo pipefail

export PLAYBOX_HOME=/tmp/tdx-gpu-run/home
export PLAYBOX_CS_JWKS=/opt/playbox/google-confidential-space-jwks.json
export PLAYBOX_GPU_JWKS=/opt/playbox/nvidia-nras-jwks.json
export PLAYBOX_CS_AUDIENCE="${PLAYBOX_CS_AUDIENCE:-playbox-t3-tdx-gpu-v0.5}"
export PLAYBOX_CS_PROJECTS="${PLAYBOX_CS_PROJECTS:-secops1219}"
export PLAYBOX_GPU_EVIDENCE_CMD=/usr/local/bin/gpu-cc-evidence.py
export PLAYBOX_GPU_COUNT="${PLAYBOX_GPU_COUNT:-1}"
export ATLASKV_MODEL="${ATLASKV_MODEL:-Qwen/Qwen3-1.7B-Base}"
export GCS_BUCKET="${GCS_BUCKET:-playbox-judge-secops1219}"
export GCS_OUT_PREFIX="${GCS_OUT_PREFIX:-t3-tdx-gpu-atlaskv}"

OUT=/tmp/tdx-gpu-run/out
mkdir -p "$PLAYBOX_HOME" "$OUT"
LOG="$OUT/workload.log"
exec > >(tee -a "$LOG") 2>&1

upload_outputs() {
  local tok
  tok=$(curl -fsS -H "Metadata-Flavor: Google" \
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token" \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])" 2>/dev/null || true)
  [ -n "$tok" ] || return 0
  for f in "$OUT"/* "$OUT"/atlaskv/summary.json; do
    [ -f "$f" ] || continue
    local bn
    bn=$(basename "$f")
    [ "$bn" = "summary.json" ] && bn="atlaskv-summary.json"
    curl -fsS -X POST -H "Authorization: Bearer $tok" -H "Content-Type: application/octet-stream" \
      --data-binary "@$f" \
      "https://storage.googleapis.com/upload/storage/v1/b/${GCS_BUCKET}/o?uploadType=media&name=${GCS_OUT_PREFIX}/${bn}" >/dev/null \
      && echo "Uploaded gs://${GCS_BUCKET}/${GCS_OUT_PREFIX}/${bn}" || true
  done
}
trap upload_outputs EXIT

echo "=== [1/5] GPU & TDX environment check ==="
nvidia-smi --query-gpu=name,driver_version,vbios_version --format=csv,noheader || true
ls -la /run/container_launcher/teeserver.sock || true

echo "=== [2/5] Refresh live NVIDIA NRAS JWKS (fallback to pinned) ==="
curl -fsSL --max-time 10 https://nras.attestation.nvidia.com/.well-known/jwks.json -o "$OUT/nvidia-nras-jwks.json" \
  && export PLAYBOX_GPU_JWKS="$OUT/nvidia-nras-jwks.json" \
  || cp "$PLAYBOX_GPU_JWKS" "$OUT/nvidia-nras-jwks.json"
cp "$PLAYBOX_CS_JWKS" "$OUT/google-cs-jwks.json"

echo "=== [3/5] Pre-fetch model & run Parametric AtlasKV eval on CUDA H100 ==="
PYTHONPATH=/opt/playbox/hermes-playbox-backend python3 - <<'PY'
import json, os, torch
from pathlib import Path
from huggingface_hub import snapshot_download
model = os.environ["ATLASKV_MODEL"]
print("cuda_available:", torch.cuda.is_available(), "device:", torch.cuda.get_device_name(0) if torch.cuda.is_available() else "cpu")
Path("/tmp/tdx-gpu-run/out/gpu-info.json").write_text(json.dumps({
    "cuda_available": torch.cuda.is_available(),
    "device_name": torch.cuda.get_device_name(0) if torch.cuda.is_available() else "cpu",
    "torch_version": torch.__version__,
    "model": model,
}, indent=2))
snapshot_download(repo_id=model)
PY
PYTHONPATH=/opt/playbox/hermes-playbox-backend python3 /opt/playbox/run_parametric_atlaskv_eval.py \
  --model "$ATLASKV_MODEL" --outdir "$OUT/atlaskv"

echo "=== [4/5] Run combined tdx+gpu hardware attestation ==="
ATT_FILE="$PLAYBOX_HOME/history/default.log"
mkdir -p "$(dirname "$ATT_FILE")"
if [ -S /run/container_launcher/teeserver.sock ]; then
  PLAYBOX_ATTEST=tdx+gpu /usr/local/bin/playbox attest --platform tdx+gpu --log "$ATT_FILE" 2>&1 | tee "$OUT/tdx-gpu-attest.log"
else
  echo "WARNING: teeserver.sock absent; running nvidia-gpu only"
  PLAYBOX_ATTEST=gpu /usr/local/bin/playbox attest --platform gpu --log "$ATT_FILE" 2>&1 | tee "$OUT/tdx-gpu-attest.log"
fi
cp "$ATT_FILE" "$OUT/tdx-gpu-history.log"

echo "=== [5/5] Export & offline-verify T3 tdx+gpu 5-gate bundle ==="
SIGN_KEY="$OUT/gate.ed25519"
python3 -c "import os; open('$SIGN_KEY','w').write(os.urandom(32).hex())"
/usr/local/bin/playbox gate export \
  --attestation "$ATT_FILE" \
  --sign-key "$SIGN_KEY" \
  --out "$OUT/playbox-t3-tdx-gpu-5gate-bundle.json"
rm -f "$SIGN_KEY"
REQ_TIER="t1"
if [ -n "${PLAYBOX_CS_IMAGES:-}" ] && [ -n "${PLAYBOX_GPU_DRIVERS:-}" ] && [ -n "${PLAYBOX_GPU_VBIOS:-}" ]; then
  REQ_TIER="t3"
fi
/usr/local/bin/playbox gate verify "$OUT/playbox-t3-tdx-gpu-5gate-bundle.json" \
  --require-signature --require-pq --require-tier "$REQ_TIER" --json \
  | tee "$OUT/t3-verify.json"
