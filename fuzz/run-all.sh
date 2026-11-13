#!/bin/sh
# Run every fuzz target for N seconds (default 60). Stops at the first crash.
set -e
cd "$(dirname "$0")"
secs="${1:-60}"
for t in wire_frame grant_token revocation_notice attestation_statement history_and_audit links_and_pem tpm_evidence tdx_token latent_envelope latent_store capability_bind room_state_files; do
  echo "=== $t (${secs}s)"
  cargo +nightly fuzz run "$t" -- -max_total_time="$secs" -timeout=10 -rss_limit_mb=3000 2>&1 | grep -E "DONE|ERROR|SUMMARY|panicked" || true
  [ -z "$(ls fuzz/artifacts/$t 2>/dev/null; ls artifacts/$t 2>/dev/null)" ] || { echo "CRASH in $t: see artifacts/$t"; exit 1; }
done
echo "no crashes"
