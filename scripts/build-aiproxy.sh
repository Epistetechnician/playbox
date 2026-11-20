#!/usr/bin/env bash
# Reproducible, CGO-free hardened build of playbox-aiproxy (`PROXY-7`).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${ROOT}/vendor/playbox-aiproxy/core"
OUT="${1:-${ROOT}/target/playbox-aiproxy}"
mkdir -p "$(dirname "${OUT}")"

cd "${SRC}"
CGO_ENABLED=0 go build \
  -tags playbox_hardened \
  -trimpath \
  -buildvcs=false \
  -ldflags "-s -w" \
  -o "${OUT}" .

SHA="$(shasum -a 256 "${OUT}" | awk '{print $1}')"
echo "Built hardened playbox-aiproxy: ${OUT}"
echo "sha256:${SHA}"
