#!/usr/bin/env bash
# End-to-end Independent-Operator Two-Key F3 Quorum Promotion (Mac Host A <-> Linux RPi5 Host B).
#
# Proves Playbox Milestone N2:
#   - Host A (macOS Apple Silicon, Operator 1 key, own sealed holdout seed, PB_TIER=1)
#   - Host B (Linux RPi5 aarch64, Operator 2 key, own sealed holdout seed, PB_TIER=2)
#   - RoomLog enforces j=2 distinct hosts, hybrid Ed25519+ML-DSA-65 cosigning, and
#     cross-machine F3 tokens-to-target agreement within 50,000 ppm:
#       1. dev_memorizer_INJECTED_BAD -> REFUSED (caught on sealed holdouts)
#       2. lr_3e-3                    -> PROMOTED (2-of-2 quorum, frontier > 2,000,000)
#       3. baseline.py                -> REFUSED (below new frontier)
#   - Both hosts' roomlog.jsonl verify offline with `playbox room-log` and
#     export into an offline-verifiable 5-gate PQ compliance bundle (`playbox gate verify`).
set -euo pipefail

RPI="${RPI_HOST:-patelsh@192.168.4.20}"
PB_MAC="${PLAYBOX_BIN:-./target/debug/playbox}"
PB_RPI="${PLAYBOX_RPI_BIN:-/home/patelsh/playbox-v046/playbox}"
OUT_DIR="${OUT_DIR:-/tmp/f3-two-key-e2e-out}"
mkdir -p "$OUT_DIR"

say() { printf '\n==> %s\n' "$*"; }
ok()  { printf '    [OK] %s\n' "$*"; }
die() { printf '    [FAIL] %s\n' "$*" >&2; exit 1; }

WORK=$(mktemp -d /tmp/playbox-f3-2key-XXXXXX)
HOST_A_PID=""
cleanup() {
  [ -n "$HOST_A_PID" ] && kill "$HOST_A_PID" 2>/dev/null || true
  ssh -o BatchMode=yes "$RPI" 'pkill -f "playbox.*f3-2key-state-b" 2>/dev/null || true' 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
say "1. Prepare F3 candidates (baseline, lr_3e-3, dev_memorizer) on Mac and RPi5"
mkdir -p "$WORK/cands-a" "$WORK/state-a" "$WORK/repo"
git init -q -b main "$WORK/repo"
cp challenge/f3/baseline.py "$WORK/repo/candidate.py"
(
  cd "$WORK/repo"
  git config user.email "f3@playbox.local"
  git config user.name "F3 Quorum"
  git add candidate.py
  git commit -qm "f3 baseline"
)

python3 - "$WORK" <<'PY'
import pathlib, sys
w = pathlib.Path(sys.argv[1])
base = pathlib.Path("challenge/f3/baseline.py").read_text()
(w / "baseline.py").write_text(base)
(w / "lr_3e-3.py").write_text(base.replace("LR = 1e-3", "LR = 3e-3"))
(w / "memorizer.py").write_text(
    base.replace(
        "x, y = ctx.next_batch(BATCH)",
        "import numpy as np\n"
        "        _i = np.random.randint(0, len(ctx.dev_blocks), BATCH)\n"
        "        _b = torch.from_numpy(ctx.dev_blocks[_i]).long()\n"
        "        ctx.batches += 1\n"
        "        ctx.tokens += BATCH * ctx.block_size\n"
        "        x, y = _b[:, :-1], _b[:, 1:]",
    )
)
PY

H_BASE=$(PLAYBOX_CANDIDATES="$WORK/cands-a" python3 challenge/room_eval.py put "$WORK/baseline.py")
H_GOOD=$(PLAYBOX_CANDIDATES="$WORK/cands-a" python3 challenge/room_eval.py put "$WORK/lr_3e-3.py")
H_BAD=$(PLAYBOX_CANDIDATES="$WORK/cands-a" python3 challenge/room_eval.py put "$WORK/memorizer.py")
RULE_LINE=$(python3 challenge/room_eval.py rule challenge/f3/task_spec.json)
PROB_HASH=$(printf '%s\n' "$RULE_LINE" | awk '{print $2}')
ok "problem=$PROB_HASH"
ok "candidates: base=${H_BASE:0:12} good=${H_GOOD:0:12} memorizer=${H_BAD:0:12}"

# Sync evaluator + candidates to RPi5
ssh -o BatchMode=yes "$RPI" 'rm -rf /tmp/f3-2key-state-b /tmp/f3-2key-cands-b /tmp/f3-2key-repo && mkdir -p /tmp/f3-2key-cands-b /tmp/f3-2key-repo /tmp/f3-rpi5/challenge/f3 /tmp/f3-rpi5/challenge/tier_a_matmul /tmp/f3-rpi5/challenge/tier_b_lm'
scp -q challenge/room_eval.py "$RPI:/tmp/f3-rpi5/challenge/"
scp -q challenge/f3/task.py challenge/f3/task_spec.json challenge/f3/baseline.py "$RPI:/tmp/f3-rpi5/challenge/f3/"
scp -q challenge/tier_b_lm/problem.json "$RPI:/tmp/f3-rpi5/challenge/tier_b_lm/"
scp -q "$WORK/baseline.py" "$WORK/lr_3e-3.py" "$WORK/memorizer.py" "$RPI:/tmp/f3-2key-cands-b/"
ssh -o BatchMode=yes "$RPI" '
  PLAYBOX_CANDIDATES=/tmp/f3-2key-cands-b /home/patelsh/f3-venv/bin/python /tmp/f3-rpi5/challenge/room_eval.py put /tmp/f3-2key-cands-b/baseline.py >/dev/null
  PLAYBOX_CANDIDATES=/tmp/f3-2key-cands-b /home/patelsh/f3-venv/bin/python /tmp/f3-rpi5/challenge/room_eval.py put /tmp/f3-2key-cands-b/lr_3e-3.py >/dev/null
  PLAYBOX_CANDIDATES=/tmp/f3-2key-cands-b /home/patelsh/f3-venv/bin/python /tmp/f3-rpi5/challenge/room_eval.py put /tmp/f3-2key-cands-b/memorizer.py >/dev/null
  git init -q -b main /tmp/f3-2key-repo
  cp /tmp/f3-2key-cands-b/baseline.py /tmp/f3-2key-repo/candidate.py
  git -C /tmp/f3-2key-repo config user.email "f3-rpi5@playbox.local"
  git -C /tmp/f3-2key-repo config user.name "F3 RPi5 Operator"
  git -C /tmp/f3-2key-repo add candidate.py
  git -C /tmp/f3-2key-repo commit -qm "f3 baseline"
'

# Evaluator wrappers: distinct sealed holdout seeds & attestation tiers per operator
cat > "$WORK/eval-a.sh" <<EOF
#!/bin/sh
export PLAYBOX_CANDIDATES="$WORK/cands-a"
export PLAYBOX_F3_TARGET=3.25
export PLAYBOX_F3_HOLDOUT_SEED=77101
export PB_TIER=1
exec python3 "$(pwd)/challenge/room_eval.py" "\$@"
EOF
chmod +x "$WORK/eval-a.sh"

ssh -o BatchMode=yes "$RPI" 'cat > /tmp/f3-rpi5/eval-b.sh << "EOF"
#!/bin/sh
export PLAYBOX_CANDIDATES=/tmp/f3-2key-cands-b
export PLAYBOX_F3_TARGET=3.25
export PLAYBOX_F3_HOLDOUT_SEED=88202
export PB_TIER=2
exec /home/patelsh/f3-venv/bin/python /tmp/f3-rpi5/challenge/room_eval.py "$@"
EOF
chmod +x /tmp/f3-rpi5/eval-b.sh'

# ---------------------------------------------------------------------------
say "2. Bootstrap Host A root key & peer addr, then start independent Host B (RPi5) replica"
"$PB_MAC" up --transport iroh --home "$WORK/home-a" --state "$WORK/state-a" --repo "$WORK/repo" > "$WORK/init-a.log" 2>&1 &
INIT_PID=$!
HOST_A_PEER=""
for _ in $(seq 1 80); do
  HOST_A_PEER=$(awk '/^peer address/{print $NF; exit}' "$WORK/init-a.log" 2>/dev/null || true)
  [ -f "$WORK/state-a/root.key" ] && [ -n "$HOST_A_PEER" ] && break
  sleep 0.25
done
kill "$INIT_PID" 2>/dev/null || true
wait "$INIT_PID" 2>/dev/null || true
[ -n "$HOST_A_PEER" ] || { cat "$WORK/init-a.log"; die "Host A did not print peer address"; }

ROOT_PUB=$(python3 - "$WORK/state-a/root.key" <<'PY'
import sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
seed = bytes.fromhex(open(sys.argv[1]).read().strip())
print(Ed25519PrivateKey.from_private_bytes(seed).public_key().public_bytes_raw().hex())
PY
)
ok "Room root pubkey: $ROOT_PUB"
ok "Host A (Mac) peer=${HOST_A_PEER:0:36}..."

# Start Host B (RPi5) over iroh following Host A's RoomLog
ssh -o BatchMode=yes "$RPI" "nohup $PB_RPI up --transport iroh --home /tmp/f3-2key-home-b --state /tmp/f3-2key-state-b --repo /tmp/f3-2key-repo --replica-of $ROOT_PUB --room-log-leader $HOST_A_PEER --room-log-evaluator /tmp/f3-rpi5/eval-b.sh > /tmp/f3-2key-b.log 2>&1 < /dev/null &"

RPI_PEER=""
for _ in $(seq 1 60); do
  RPI_PEER=$(ssh -o BatchMode=yes "$RPI" 'awk "/^peer address/{print \$NF; exit}" /tmp/f3-2key-b.log' || true)
  [ -n "$RPI_PEER" ] && break
  sleep 0.5
done
[ -n "$RPI_PEER" ] || { ssh -o BatchMode=yes "$RPI" 'cat /tmp/f3-2key-b.log'; die "Host B (RPi5) did not print peer address"; }
ok "Host B (RPi5) online; peer=${RPI_PEER:0:36}..."

# ---------------------------------------------------------------------------
say "3. Start Host A (Mac) leader paired with Host B (RPi5) & run 2-key F3 quorum"
mkfifo "$WORK/console.in"
exec 3<>"$WORK/console.in"

"$PB_MAC" up \
  --transport iroh \
  --console \
  --home "$WORK/home-a" \
  --state "$WORK/state-a" \
  --repo "$WORK/repo" \
  --room-log-peer "$RPI_PEER" \
  --room-log-evaluator "$WORK/eval-a.sh" \
  < "$WORK/console.in" > "$WORK/host-a.log" 2>&1 &
HOST_A_PID=$!

for _ in $(seq 1 80); do
  grep -q 'host console:' "$WORK/host-a.log" && break
  kill -0 "$HOST_A_PID" 2>/dev/null || { cat "$WORK/host-a.log"; die "Host A exited early"; }
  sleep 0.25
done
ok "Host A (Mac) leader online and connected to Host B (RPi5)"

wait_log() {
  local pat="$1" timeout="${2:-120}"
  for _ in $(seq 1 $((timeout * 2))); do
    grep -qE "$pat" "$WORK/host-a.log" && return 0
    sleep 0.5
  done
  cat "$WORK/host-a.log"
  die "timed out waiting for pattern: $pat"
}

# Register F3 problem (j=2 hosts, 50,000 ppm noise, 50,000 ppm min gain, baseline 1,000,000)
printf '%s\n' "$RULE_LINE" >&3
wait_log "problem registered" 30
ok "registered F3 problem in 2-host RoomLog (j=2)"

# Attempt 1: dev_memorizer_INJECTED_BAD (must be REFUSED!)
printf 'roomlog-promote %s %s\n' "$PROB_HASH" "$H_BAD" >&3
wait_log "not promoted:" 120
BAD_LINE=$(grep "not promoted:" "$WORK/host-a.log" | tail -n 1)
ok "Attempt 1 (dev_memorizer): $BAD_LINE"

# Attempt 2: lr_3e-3 (must be PROMOTED by 2-of-2 quorum Mac + RPi5!)
printf 'roomlog-promote %s %s\n' "$PROB_HASH" "$H_GOOD" >&3
wait_log "promoted; the frontier metric is now" 180
GOOD_LINE=$(grep "promoted; the frontier metric is now" "$WORK/host-a.log" | tail -n 1)
ok "Attempt 2 (lr_3e-3, 2-of-2 Mac+RPi5 quorum): $GOOD_LINE"

# Attempt 3: baseline.py (must be REFUSED — below new frontier!)
BEFORE_FAILS=$(grep -c "not promoted:" "$WORK/host-a.log" || true)
printf 'roomlog-promote %s %s\n' "$PROB_HASH" "$H_BASE" >&3
for _ in $(seq 1 240); do
  NOW_FAILS=$(grep -c "not promoted:" "$WORK/host-a.log" || true)
  [ "$NOW_FAILS" -gt "$BEFORE_FAILS" ] && break
  sleep 0.5
done
[ "$(grep -c 'not promoted:' "$WORK/host-a.log" || true)" -gt "$BEFORE_FAILS" ] || { cat "$WORK/host-a.log"; die "Attempt 3 did not finish"; }
BASE_LINE=$(grep "not promoted:" "$WORK/host-a.log" | tail -n 1)
ok "Attempt 3 (baseline below frontier): $BASE_LINE"

# Print roomlog status and quit cleanly
printf 'roomlog\nquit\n' >&3
wait "$HOST_A_PID" 2>/dev/null || true
HOST_A_PID=""
exec 3>&-

# ---------------------------------------------------------------------------
say "4. Verify RoomLog offline on BOTH machines & export 5-gate PQ bundle"
cp "$WORK/state-a/roomlog.jsonl" "$OUT_DIR/host-a-roomlog.jsonl"
scp -q "$RPI:/tmp/f3-2key-state-b/roomlog.jsonl" "$OUT_DIR/host-b-roomlog.jsonl"

echo "--- Mac (Host A) offline room-log check ---"
"$PB_MAC" room-log "$OUT_DIR/host-a-roomlog.jsonl" --root "$ROOT_PUB"
echo "--- RPi5 (Host B) offline room-log check ---"
"$PB_MAC" room-log "$OUT_DIR/host-b-roomlog.jsonl" --root "$ROOT_PUB"

"$PB_MAC" release pq-keygen "$WORK/bundle.pq.key" >/dev/null
"$PB_MAC" gate export \
  --home "$WORK/home-a" \
  --state "$WORK/state-a" \
  --pq-key "$WORK/bundle.pq.key" \
  --out "$OUT_DIR/f3-two-key-5gate-bundle.json"

VERIFY_JSON=$("$PB_MAC" gate verify \
  --home "$WORK/home-a" \
  "$OUT_DIR/f3-two-key-5gate-bundle.json" \
  --require-signature \
  --require-pq \
  --json)
printf '%s\n' "$VERIFY_JSON" > "$OUT_DIR/verify.json"
ok "5-gate hybrid PQ compliance bundle verified offline!"
printf '%s\n' "$VERIFY_JSON"
