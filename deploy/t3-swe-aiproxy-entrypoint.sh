#!/usr/bin/env bash
set -euo pipefail

export PLAYBOX_BIN=/usr/local/bin/playbox
export PLAYBOX_CS_JWKS=/opt/playbox/google-confidential-space-jwks.json
export PLAYBOX_CS_AUDIENCE="${PLAYBOX_CS_AUDIENCE:-playbox-t3-swe-aiproxy-v0.5}"
export PLAYBOX_CS_PROJECTS="${PLAYBOX_CS_PROJECTS:-secops1219}"
export PLAYBOX_GATE_REQUIRE_TIER="${PLAYBOX_GATE_REQUIRE_TIER:-t3}"
export GCP_PROJECT="${GCP_PROJECT:-secops1219}"
export AIPROXY_PORT="${AIPROXY_PORT:-18791}"
export AIPROXY_ADMIN_KEY="${AIPROXY_ADMIN_KEY:-pbx-tdx-admin-ephemeral}"
export GCS_BUCKET="${GCS_BUCKET:-playbox-judge-secops1219}"
export GCS_OUT_PREFIX="${GCS_OUT_PREFIX:-t3-swe-aiproxy-v05}"
export SWE_CONCURRENCY="${SWE_CONCURRENCY:-2}"
export SWE_TIMEOUT="${SWE_TIMEOUT:-420}"
export SWE_T1_MODEL="${SWE_T1_MODEL:-gemini-3.1-pro-preview}"
export SWE_T2_MODEL="${SWE_T2_MODEL:-gemini-3.1-pro-preview}"
export SWE_MAX_TIERS="${SWE_MAX_TIERS:-1}"
export REMOTE_GRADER_SSH="${REMOTE_GRADER_SSH:-}"
export UPSTREAM_BASE_URL="${UPSTREAM_BASE_URL:-}"
export UPSTREAM_API_KEY="${UPSTREAM_API_KEY:-}"
export UPSTREAM_INPUT_PRICE="${UPSTREAM_INPUT_PRICE:-0.0002}"
export UPSTREAM_OUTPUT_PRICE="${UPSTREAM_OUTPUT_PRICE:-0.0008}"
export VERTEX_BASE_URL="https://aiplatform.googleapis.com/v1beta1/projects/${GCP_PROJECT}/locations/global/endpoints/openapi"
export PB_SWE_DIR=/opt/playbox/swe-slice
export PB_HERMES_PY=/opt/hermes-venv/bin/python
export PB_HERMES_AGENT_DIR=/opt/hermes-agent
export PB_SKIP_ONLINE_GATE=1
export HERMES_DISABLE_LAZY_INSTALLS=1

mkdir -p /tmp/swe-aiproxy-tdx/out /tmp/swe-aiproxy-tdx/aiproxy /root/.ssh
chmod 700 /root/.ssh
LOG=/tmp/swe-aiproxy-tdx/out/workload.log
exec > >(tee -a "$LOG") 2>&1

if [ -n "${SSH_KEY_B64:-}" ]; then
  printf '%s' "$SSH_KEY_B64" | base64 -d > /root/.ssh/google_compute_engine
  chmod 600 /root/.ssh/google_compute_engine
  unset SSH_KEY_B64
fi
if [ -n "$REMOTE_GRADER_SSH" ]; then
  export PB_REMOTE_GCE_SSH="$REMOTE_GRADER_SSH"
fi

fetch_gcp_oauth_token() {
  curl -fsS -H "Metadata-Flavor: Google" \
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token" \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])"
}

upload_file_gcs() {
  local f="$1"
  [ -f "$f" ] || return 0
  local tok
  tok=$(fetch_gcp_oauth_token 2>/dev/null || true)
  [ -n "$tok" ] || return 0
  local bn
  bn=$(basename "$f")
  curl -fsS -X POST -H "Authorization: Bearer $tok" -H "Content-Type: application/octet-stream" \
    --data-binary "@$f" \
    "https://storage.googleapis.com/upload/storage/v1/b/${GCS_BUCKET}/o?uploadType=media&name=${GCS_OUT_PREFIX}/${bn}" >/dev/null || true
}

echo "=== [1/5] Start hardened playbox-aiproxy on 127.0.0.1:${AIPROXY_PORT} ==="
printf '%s' "$AIPROXY_ADMIN_KEY" > /tmp/swe-aiproxy-tdx/admin.key
chmod 600 /tmp/swe-aiproxy-tdx/admin.key

SQL_DSN="file:/tmp/swe-aiproxy-tdx/aiproxy/aiproxy.db?cache=shared&_busy_timeout=10000&_journal_mode=WAL" \
  ADMIN_KEY="$AIPROXY_ADMIN_KEY" \
  LISTEN="127.0.0.1:${AIPROXY_PORT}" \
  /usr/local/bin/playbox-aiproxy > /tmp/swe-aiproxy-tdx/out/aiproxy.log 2>&1 &
AIPROXY_PID=$!

REFRESH_PID=""
cleanup() {
  [ -n "$REFRESH_PID" ] && kill "$REFRESH_PID" 2>/dev/null || true
  kill "$AIPROXY_PID" 2>/dev/null || true
  for cf in /tmp/swe-aiproxy-tdx/out/playbox-t3-aiproxy-5gate-bundle.json \
            /tmp/swe-aiproxy-tdx/out/eval/verified-eval-5gate-bundle.json \
            /tmp/swe-aiproxy-tdx/out/t3-verify.json \
            /tmp/swe-aiproxy-tdx/out/tdx-attest.log \
            /tmp/swe-aiproxy-tdx/out/eval/verified-eval-summary.json \
            /tmp/swe-aiproxy-tdx/out/eval/checkpoint.jsonl \
            /tmp/swe-aiproxy-tdx/out/aiproxy.log \
            "$LOG"; do
    upload_file_gcs "$cf"
  done
}
trap cleanup EXIT

for _ in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:${AIPROXY_PORT}/api/status" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "=== [2/5] Register model configs + upstream channel in playbox-aiproxy ==="
if [ -n "$UPSTREAM_BASE_URL" ]; then
  UP_KEY="${UPSTREAM_API_KEY:-sk-local}"
  curl -fsS -X POST "http://127.0.0.1:${AIPROXY_PORT}/api/model_configs/" \
    -H "Authorization: ${AIPROXY_ADMIN_KEY}" \
    -H "Content-Type: application/json" \
    -d "[
      {\"model\":\"${SWE_T1_MODEL}\",\"owner\":\"open-weights\",\"type\":1,\"price\":{\"input_price\":${UPSTREAM_INPUT_PRICE},\"output_price\":${UPSTREAM_OUTPUT_PRICE}}}
    ]"
  echo ""
  CH_RESP=$(curl -fsS -X POST "http://127.0.0.1:${AIPROXY_PORT}/api/channel/" \
    -H "Authorization: ${AIPROXY_ADMIN_KEY}" \
    -H "Content-Type: application/json" \
    -d "{
      \"type\": 1,
      \"name\": \"open-weights-vllm\",
      \"base_url\": \"${UPSTREAM_BASE_URL}\",
      \"key\": \"${UP_KEY}\",
      \"models\": [\"${SWE_T1_MODEL}\"]
    }")
  echo "Channel response: ${CH_RESP}"
  unset UPSTREAM_API_KEY UP_KEY
  (
    while true; do
      sleep 300
      upload_file_gcs /tmp/swe-aiproxy-tdx/out/eval/checkpoint.jsonl
      upload_file_gcs "$LOG"
    done
  ) &
  REFRESH_PID=$!
else
  # Register Vertex Gemini 3 + GPT-OSS-120B MaaS model configs BEFORE creating the channel so host_aiproxy meters exact USD.
  curl -fsS -X POST "http://127.0.0.1:${AIPROXY_PORT}/api/model_configs/" \
    -H "Authorization: ${AIPROXY_ADMIN_KEY}" \
    -H "Content-Type: application/json" \
    -d '[
      {"model":"gemini-3.1-pro-preview","owner":"google","type":1,"price":{"input_price":0.002,"output_price":0.012}},
      {"model":"gemini-3-flash-preview","owner":"google","type":1,"price":{"input_price":0.0005,"output_price":0.003}},
      {"model":"gpt-oss-120b-maas","owner":"openai","type":1,"price":{"input_price":0.00015,"output_price":0.0006}}
    ]'
  echo ""

  GCP_TOKEN="${GCP_OAUTH_TOKEN:-$(fetch_gcp_oauth_token)}"
  CH_RESP=$(curl -fsS -X POST "http://127.0.0.1:${AIPROXY_PORT}/api/channel/" \
    -H "Authorization: ${AIPROXY_ADMIN_KEY}" \
    -H "Content-Type: application/json" \
    -d "{
      \"type\": 1,
      \"name\": \"vertex-gemini3-oauth\",
      \"base_url\": \"${VERTEX_BASE_URL}\",
      \"key\": \"${GCP_TOKEN}\",
      \"models\": [\"gemini-3-flash-preview\", \"gemini-3.1-pro-preview\", \"gpt-oss-120b-maas\"],
      \"model_mapping\": {
        \"gemini-3-flash-preview\": \"google/gemini-3-flash-preview\",
        \"gemini-3.1-pro-preview\": \"google/gemini-3.1-pro-preview\",
        \"gpt-oss-120b-maas\": \"openai/gpt-oss-120b-maas\"
      }
    }")
  echo "Channel response: ${CH_RESP}"
  curl -fsS -X POST "http://127.0.0.1:${AIPROXY_PORT}/api/group/bench%3Asolver" \
    -H "Authorization: ${AIPROXY_ADMIN_KEY}" \
    -H "Content-Type: application/json" \
    -d '{"rpm_ratio":1.0,"tpm_ratio":1.0}' >/dev/null || true
  CH_ID=$(printf '%s' "$CH_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data',{}).get('id','1'))" 2>/dev/null || echo "1")
  echo "Registered Vertex OAuth channel id=${CH_ID}"

  # Refresh CVM OAuth token every 3 minutes (GCP metadata caches until <5m left) + checkpoint every 15m.
  (
    CUR_TOK="$GCP_TOKEN"
    TICK=0
    while true; do
      sleep 180
      TICK=$((TICK + 1))
      NEW_TOK=$(fetch_gcp_oauth_token 2>/dev/null || true)
      if [ -n "$NEW_TOK" ] && [ "$NEW_TOK" != "$CUR_TOK" ]; then
        if curl -fsS -X PUT "http://127.0.0.1:${AIPROXY_PORT}/api/channel/${CH_ID}" \
          -H "Authorization: ${AIPROXY_ADMIN_KEY}" \
          -H "Content-Type: application/json" \
          -d "{\"key\": \"${NEW_TOK}\"}" >/dev/null 2>&1; then
          CUR_TOK="$NEW_TOK"
          echo "[oauth-refresh] updated channel ${CH_ID} token at $(date -u +%H:%M:%SZ)"
        fi
      fi
      if [ $((TICK % 5)) -eq 0 ]; then
        upload_file_gcs /tmp/swe-aiproxy-tdx/out/eval/checkpoint.jsonl
        upload_file_gcs "$LOG"
      fi
    done
  ) &
  REFRESH_PID=$!
fi

echo "=== [3/5] Live Intel TDX hardware attestation ==="
ls -la /run/container_launcher/teeserver.sock || true
if [ -S /run/container_launcher/teeserver.sock ]; then
  export PLAYBOX_ATTEST=tdx
  mkdir -p /root/.playbox/history
  /usr/local/bin/playbox attest --platform tdx --log /root/.playbox/history/default.log
  cp /root/.playbox/history/default.log /tmp/swe-aiproxy-tdx/out/tdx-attest.log
  tail -n 3 /root/.playbox/history/default.log
fi

echo "=== [4/5] Run live in-TDX SWE-bench slice through playbox-aiproxy ==="
PYTHONPATH=/opt/playbox/hermes-playbox-backend:/opt/playbox/tools/bench/hermes-parity \
  python3 /opt/playbox/tools/bench/hermes-parity/run_verified_eval.py \
    --arms P2 \
    --instances all-swe-dir \
    --runtime process \
    --no-warm-pool \
    --concurrency "$SWE_CONCURRENCY" \
    --timeout "$SWE_TIMEOUT" \
    --max-tiers "$SWE_MAX_TIERS" \
    --no-t1-repro-gate \
    --t1-harness hermes \
    --t1-provider custom \
    --t1-model "$SWE_T1_MODEL" \
    --t2-harness hermes \
    --t2-provider custom \
    --t2-model "$SWE_T2_MODEL" \
    --critic-model "" \
    --aiproxy-url "http://127.0.0.1:${AIPROXY_PORT}" \
    --aiproxy-admin-key-file /tmp/swe-aiproxy-tdx/admin.key \
    --aiproxy-failover-policy same_model_only \
    --aiproxy-cache-mode off \
    --require-host-aiproxy \
    --out /tmp/swe-aiproxy-tdx/out/eval

echo "=== [5/5] Verify aggregated T3 + host_aiproxy 5-gate bundle & upload to GCS ==="
BUNDLE=/tmp/swe-aiproxy-tdx/out/eval/verified-eval-5gate-bundle.json
if [ -f "$BUNDLE" ]; then
  /usr/local/bin/playbox gate verify "$BUNDLE" \
    --require-signature --require-pq --require-tier "$PLAYBOX_GATE_REQUIRE_TIER" --json \
    | tee /tmp/swe-aiproxy-tdx/out/t3-verify.json
  cp "$BUNDLE" /tmp/swe-aiproxy-tdx/out/playbox-t3-aiproxy-5gate-bundle.json
fi

for f in /tmp/swe-aiproxy-tdx/out/playbox-t3-aiproxy-5gate-bundle.json \
         /tmp/swe-aiproxy-tdx/out/t3-verify.json \
         /tmp/swe-aiproxy-tdx/out/tdx-attest.log \
         /tmp/swe-aiproxy-tdx/out/workload.log \
         /tmp/swe-aiproxy-tdx/out/aiproxy.log \
         /tmp/swe-aiproxy-tdx/out/eval/verified-eval-summary.json \
         /tmp/swe-aiproxy-tdx/out/eval/checkpoint.jsonl; do
  upload_file_gcs "$f" && [ -f "$f" ] && echo "Uploaded gs://${GCS_BUCKET}/${GCS_OUT_PREFIX}/$(basename "$f")"
done
