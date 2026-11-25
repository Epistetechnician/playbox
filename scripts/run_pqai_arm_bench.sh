#!/usr/bin/env bash
set -euo pipefail

# Reproducible Layer 2A PQAI Home-Staker Benchmark Runner
# Targets: Apple Silicon ARM64, AWS Graviton4 (c8g), Raspberry Pi 5 (Cortex-A76)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLAYBOX_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUT_DIR="${1:-/tmp/pqai_arm_bench_$(uname -m)_$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "${OUT_DIR}"

ARCH="$(uname -m)"
OS="$(uname -s)"
CPU_MODEL="$(lscpu 2>/dev/null | awk -F: '/Model name/{gsub(/^[ \t]+/, "", $2); print $2; exit}' || sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
CORES="$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 1)"
RAM_MB="$(free -m 2>/dev/null | awk '/Mem:/{print $2}' || sysctl -n hw.memsize 2>/dev/null | awk '{printf "%.0f", $1/1048576}' || echo 0)"

cat > "${OUT_DIR}/host_metadata.json" <<EOF
{
  "os": "${OS}",
  "arch": "${ARCH}",
  "cpu_model": "${CPU_MODEL}",
  "cores": ${CORES},
  "ram_mb": ${RAM_MB},
  "timestamp_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF

echo "[1/3] Running PQAI home-staker signature benchmark (Ed25519, ML-DSA-65, XMSS-WOTS+-SHA256, SLH-DSA-SHA2-128f)..."
(
  cd "${PLAYBOX_ROOT}"
  cargo run --release -p playbox-core --example pqai_home_staker_bench -- \
    --json-out "${OUT_DIR}/signatures.json"
)

LABINIUS_DIR="${PLAYBOX_ROOT}/.devin/yukon-labinius-arm/solution-arm/labinius"
if [[ -d "${LABINIUS_DIR}" ]]; then
  echo "[2/3] Running hash-based PCS competitors (WHIR, BaseFold, Brakedown, Ligerito) suite s..."
  (
    cd "${LABINIUS_DIR}"
    RAYON_NUM_THREADS=1 cargo run --release --bin pcs-competitors -- --suite s \
      | tee "${OUT_DIR}/pcs_competitors_s.txt"
  )
  echo "[3/3] Running lattice PCS baseline (Labinius) suite s..."
  (
    cd "${LABINIUS_DIR}"
    RAYON_NUM_THREADS=1 cargo run --release --bin labinius -- --suite s \
      | tee "${OUT_DIR}/labinius_s.txt"
  )
else
  echo "[2/3] Skipping PCS competitors (labinius checkout not found at ${LABINIUS_DIR})"
fi

echo "Done. Hardware-tagged PQAI benchmark bundle written to: ${OUT_DIR}"
