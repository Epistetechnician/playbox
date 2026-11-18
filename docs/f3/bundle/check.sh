#!/usr/bin/env bash
# F3 bundle check. Tier A: no GPU, no dataset, no network, no git. Needs python3 (>= 3.10), `cryptography`, `numpy`, and shasum or sha256sum.
# Usage: ./check.sh        (run from anywhere; the bundle directory is where this file is)
# Writes nothing into the bundle. Exit 0 only if the file hashes match and both runs verify.
set -uo pipefail
B="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PYTHONDONTWRITEBYTECODE=1 F3_DATA_DIR="${TMPDIR:-/tmp}/f3-no-such-data-dir"

# The host public keys below are the pins. Do not take them from the bundle's own host.pub files; those are compared to them.
PIN1=9b2fdb975930638e802e83fc7045e2af30b3d2c813240943bb0ad446c796a3f5   # pilot 1 (recorded in pilot/README.md; key printed by init)
PIN2=e4b0b7b959914f2e3332fe7037fe4e63854a44bc87671cb53a86621055dcce5d   # pilot 2 (docs/f3/pilot2/PINNED.txt, pinned 2026-11-15T00:58Z before any candidate)

fail=0
echo "== 0. bundle file hashes (SHA256SUMS)"
cd "$B"
if command -v sha256sum >/dev/null; then sha256sum -c SHA256SUMS --quiet; else shasum -a 256 -c SHA256SUMS --quiet; fi \
  && echo "[PASS] every file matches SHA256SUMS" || { echo "[FAIL] SHA256SUMS mismatch"; fail=1; }
[ "$(find . -type f ! -name SHA256SUMS | wc -l | tr -d ' ')" = "$(wc -l < SHA256SUMS | tr -d ' ')" ] && echo "[PASS] no file outside SHA256SUMS" || { echo "[FAIL] files not listed in SHA256SUMS"; fail=1; }

run() {  # name pin pubfile protocol
  echo; echo "== $1: host key $2"
  [ "$(tr -d '[:space:]' < "$B/runs/$1/host.pub")" = "$2" ] && echo "[PASS] runs/$1/host.pub equals the pinned key" || { echo "[FAIL] host.pub differs from the pin"; fail=1; }
  python3 -I -B "$B/challenge/f3/verify.py" --work "$B/runs/$1" --host-pubkey "$2" --repo-root "$B" \
      --protocol-file "$B/protocol/PROTOCOL.$1.md" --no-data || fail=1
}
run pilot1 "$PIN1"
run pilot2 "$PIN2"
echo
if [ "$fail" = 0 ]; then echo "TIER A: ALL CHECKS PASSED (see README: what this does not cover)"; else echo "TIER A: FAILED"; fi
exit $fail
