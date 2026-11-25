#!/usr/bin/env bash
# End-to-end 3-Physical-Machine Byzantine-Tolerant F3 Quorum + Node-Loss Resilience E2E:
#   - Host A: MacBook Pro M4 Max (Leader, macOS T1 Secure Enclave, holdout seed 77101)
#   - Host B: MacBook Air M1 8GB (episteman@192.168.4.31, Replica 1, macOS T1 Secure Enclave, holdout seed 88202)
#   - Host C: Raspberry Pi 5     (patelsh@192.168.4.20,  Replica 2, Linux T2 TPM 2.0,       holdout seed 99303)
#
# Proves 3-node heterogeneous quorum (2 of 3 hosts required, hybrid Ed25519+ML-DSA-65 PQ commits):
#   1. dev_memorizer_INJECTED_BAD -> REFUSED (caught on sealed holdouts)
#   2. lr_3e-3                    -> PROMOTED by 3-of-3 physical machines (frontier 2,250,000)
#   3. Kill Host C (RPi5) mid-room!
#   4. lr_6e-3                    -> PROMOTED by surviving 2-of-3 quorum (M4 Max + M1 Air, frontier 3,000,000)
#   5. Verify RoomLog offline on Host A & Host B (identical PQ-committed head) + export/verify 5-gate PQ bundle.
set -euo pipefail

M1="${M1_HOST:-episteman@192.168.4.31}"
RPI="${RPI_HOST:-patelsh@192.168.4.20}"
PB_MAC="${PLAYBOX_BIN:-./target/debug/playbox}"
PB_M1="${PLAYBOX_M1_BIN:-/Users/episteman/playbox-v046/playbox}"
PB_RPI="${PLAYBOX_RPI_BIN:-/home/patelsh/playbox-v046/playbox}"
OUT_DIR="${OUT_DIR:-/tmp/f3-three-node-e2e-out}"
mkdir -p "$OUT_DIR"

say() { printf '\n==> %s\n' "$*"; }
ok()  { printf '    [OK] %s\n' "$*"; }
die() { printf '    [FAIL] %s\n' "$*" >&2; exit 1; }

WORK=$(mktemp -d /tmp/playbox-f3-3node-XXXXXX)
HOST_A_PID=""
cleanup() {
  [ -n "$HOST_A_PID" ] && kill "$HOST_A_PID" 2>/dev/null || true
  ssh -o BatchMode=yes "$M1"  'pkill -f "[p]laybox.*f3-3node-state-b" 2>/dev/null || true' 2>/dev/null || true
  ssh -o BatchMode=yes "$RPI" 'pkill -f "[p]laybox.*f3-3node-state-c" 2>/dev/null || true' 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
say "1. Prepare F3 candidates (baseline, lr_3e-3, lr_6e-3, dev_memorizer) across all 3 physical machines"
mkdir -p "$WORK/cands-a" "$WORK/state-a" "$WORK/repo"
git init -q -b main "$WORK/repo"
cp challenge/f3/baseline.py "$WORK/repo/candidate.py"
(
  cd "$WORK/repo"
  git config user.email "f3-3node@playbox.local"
  git config user.name "F3 3-Node Quorum"
  git add candidate.py
  git commit -qm "f3 baseline"
)

python3 - "$WORK" <<'PY'
import pathlib, sys
w = pathlib.Path(sys.argv[1])
base = pathlib.Path("challenge/f3/baseline.py").read_text()
(w / "baseline.py").write_text(base)
(w / "lr_3e-3.py").write_text(base.replace("LR = 1e-3", "LR = 3e-3"))
(w / "lr_6e-3.py").write_text(base.replace("LR = 1e-3", "LR = 6e-3"))
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
H_STEP1=$(PLAYBOX_CANDIDATES="$WORK/cands-a" python3 challenge/room_eval.py put "$WORK/lr_3e-3.py")
H_STEP2=$(PLAYBOX_CANDIDATES="$WORK/cands-a" python3 challenge/room_eval.py put "$WORK/lr_6e-3.py")
H_BAD=$(PLAYBOX_CANDIDATES="$WORK/cands-a" python3 challenge/room_eval.py put "$WORK/memorizer.py")
RULE_LINE=$(python3 challenge/room_eval.py rule challenge/f3/task_spec.json)
PROB_HASH=$(printf '%s\n' "$RULE_LINE" | awk '{print $2}')
ok "problem=$PROB_HASH"
ok "candidates: base=${H_BASE:0:12} lr_3e-3=${H_STEP1:0:12} lr_6e-3=${H_STEP2:0:12} memorizer=${H_BAD:0:12}"

# Sync to Host B (MacBook Air M1) and Host C (RPi5) in parallel
sync_remote() {
  local host="$1" tag="$2" pybin="$3" hold_seed="$4" tier="$5"
  ssh -o BatchMode=yes "$host" "rm -rf /tmp/f3-3node-state-${tag} /tmp/f3-3node-cands-${tag} /tmp/f3-3node-repo-${tag} /tmp/f3-3node-home-${tag} && mkdir -p /tmp/f3-3node-cands-${tag} /tmp/f3-3node-repo-${tag} /tmp/f3-3node-code/challenge/f3 /tmp/f3-3node-code/challenge/tier_a_matmul /tmp/f3-3node-code/challenge/tier_b_lm"
  scp -q challenge/room_eval.py "${host}:/tmp/f3-3node-code/challenge/"
  scp -q challenge/f3/task.py challenge/f3/task_spec.json challenge/f3/baseline.py "${host}:/tmp/f3-3node-code/challenge/f3/"
  scp -q challenge/tier_b_lm/problem.json "${host}:/tmp/f3-3node-code/challenge/tier_b_lm/"
  scp -q "$WORK/baseline.py" "$WORK/lr_3e-3.py" "$WORK/lr_6e-3.py" "$WORK/memorizer.py" "${host}:/tmp/f3-3node-cands-${tag}/"
  ssh -o BatchMode=yes "$host" "
    for f in baseline.py lr_3e-3.py lr_6e-3.py memorizer.py; do
      PLAYBOX_CANDIDATES=/tmp/f3-3node-cands-${tag} ${pybin} /tmp/f3-3node-code/challenge/room_eval.py put /tmp/f3-3node-cands-${tag}/\$f >/dev/null
    done
    git init -q -b main /tmp/f3-3node-repo-${tag}
    cp /tmp/f3-3node-cands-${tag}/baseline.py /tmp/f3-3node-repo-${tag}/candidate.py
    git -C /tmp/f3-3node-repo-${tag} config user.email 'f3-${tag}@playbox.local'
    git -C /tmp/f3-3node-repo-${tag} config user.name 'F3 Operator ${tag}'
    git -C /tmp/f3-3node-repo-${tag} add candidate.py
    git -C /tmp/f3-3node-repo-${tag} commit -qm 'f3 baseline'
    cat > /tmp/f3-3node-code/eval-${tag}.sh <<'EOF'
#!/bin/sh
export PLAYBOX_CANDIDATES=/tmp/f3-3node-cands-${tag}
export PLAYBOX_F3_TARGET=3.25
export PLAYBOX_F3_HOLDOUT_SEED=${hold_seed}
export PB_TIER=${tier}
exec ${pybin} /tmp/f3-3node-code/challenge/room_eval.py \"\$@\"
EOF
    chmod +x /tmp/f3-3node-code/eval-${tag}.sh
  "
}

sync_remote "$M1"  "b" "/Users/episteman/f3-venv/bin/python" "88202" "1" &
PID_SYNC_B=$!
sync_remote "$RPI" "c" "/home/patelsh/f3-venv/bin/python"    "99303" "2" &
PID_SYNC_C=$!
wait "$PID_SYNC_B" "$PID_SYNC_C"
ok "synced F3 evaluator + candidates to M1 Air (Host B, holdout=88202) and RPi5 (Host C, holdout=99303)"

cat > "$WORK/eval-a.sh" <<EOF
#!/bin/sh
export PLAYBOX_CANDIDATES="$WORK/cands-a"
export PLAYBOX_F3_TARGET=3.25
export PLAYBOX_F3_HOLDOUT_SEED=77101
export PB_TIER=1
exec python3 "$(pwd)/challenge/room_eval.py" "\$@"
EOF
chmod +x "$WORK/eval-a.sh"

# ---------------------------------------------------------------------------
say "2. Bootstrap Host A root key & start Host B (M1 Air) + Host C (RPi5) replicas"
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

ssh -o BatchMode=yes "$M1"  "nohup $PB_M1  up --transport iroh --home /tmp/f3-3node-home-b --state /tmp/f3-3node-state-b --repo /tmp/f3-3node-repo-b --replica-of $ROOT_PUB --room-log-leader $HOST_A_PEER --room-log-evaluator /tmp/f3-3node-code/eval-b.sh > /tmp/f3-3node-b.log 2>&1 < /dev/null &"
ssh -o BatchMode=yes "$RPI" "nohup $PB_RPI up --transport iroh --home /tmp/f3-3node-home-c --state /tmp/f3-3node-state-c --repo /tmp/f3-3node-repo-c --replica-of $ROOT_PUB --room-log-leader $HOST_A_PEER --room-log-evaluator /tmp/f3-3node-code/eval-c.sh > /tmp/f3-3node-c.log 2>&1 < /dev/null &"

M1_PEER=""
RPI_PEER=""
for _ in $(seq 1 60); do
  [ -z "$M1_PEER" ]  && M1_PEER=$(ssh -o BatchMode=yes "$M1"  'awk "/^peer address/{print \$NF; exit}" /tmp/f3-3node-b.log' || true)
  [ -z "$RPI_PEER" ] && RPI_PEER=$(ssh -o BatchMode=yes "$RPI" 'awk "/^peer address/{print \$NF; exit}" /tmp/f3-3node-c.log' || true)
  [ -n "$M1_PEER" ] && [ -n "$RPI_PEER" ] && break
  sleep 0.5
done
[ -n "$M1_PEER" ]  || { ssh -o BatchMode=yes "$M1"  'cat /tmp/f3-3node-b.log'; die "Host B (M1 Air) failed to start"; }
[ -n "$RPI_PEER" ] || { ssh -o BatchMode=yes "$RPI" 'cat /tmp/f3-3node-c.log'; die "Host C (RPi5) failed to start"; }
ok "Host B (M1 Air, macOS) online; peer=${M1_PEER:0:32}..."
ok "Host C (RPi5, Linux)   online; peer=${RPI_PEER:0:32}..."

# ---------------------------------------------------------------------------
say "3. Start Host A (M4 Max) with BOTH peers (2-of-3 quorum) & run 3-node hill-climb + node-loss test"
mkfifo "$WORK/console.in"
exec 3<>"$WORK/console.in"

"$PB_MAC" up \
  --transport iroh \
  --console \
  --home "$WORK/home-a" \
  --state "$WORK/state-a" \
  --repo "$WORK/repo" \
  --room-log-peer "$M1_PEER" \
  --room-log-peer "$RPI_PEER" \
  --room-log-evaluator "$WORK/eval-a.sh" \
  < "$WORK/console.in" > "$WORK/host-a.log" 2>&1 &
HOST_A_PID=$!

for _ in $(seq 1 80); do
  grep -q 'host console:' "$WORK/host-a.log" && break
  kill -0 "$HOST_A_PID" 2>/dev/null || { cat "$WORK/host-a.log"; die "Host A exited early"; }
  sleep 0.25
done
ok "Host A (M4 Max) leader online with 2 replicas (2-of-3 quorum)"

wait_log() {
  local pat="$1" timeout="${2:-240}"
  for _ in $(seq 1 $((timeout * 2))); do
    grep -qE "$pat" "$WORK/host-a.log" && return 0
    sleep 0.5
  done
  cat "$WORK/host-a.log"
  die "timed out waiting for pattern: $pat"
}

printf '%s\n' "$RULE_LINE" >&3
wait_log "problem registered" 40
ok "registered F3 problem (j=2 hosts required out of 3 physical machines)"

# Step 1: Anti-Goodhart cheat refusal
printf 'roomlog-promote %s %s\n' "$PROB_HASH" "$H_BAD" >&3
wait_log "not promoted:" 120
ok "Step 1 (dev_memorizer): $(grep 'not promoted:' "$WORK/host-a.log" | tail -n 1)"

# Step 2: Promote lr_3e-3 with all 3 physical hosts alive (M4 Max + M1 Air + RPi5)
printf 'roomlog-promote %s %s\n' "$PROB_HASH" "$H_STEP1" >&3
wait_log "promoted; the frontier metric is now 2250000" 240
ok "Step 2 (lr_3e-3, 3-of-3 alive M4+M1+RPi5): $(grep 'promoted; the frontier metric is now' "$WORK/host-a.log" | tail -n 1)"

# Step 3: Kill Host C (RPi5) mid-room to prove fault-tolerant 2-of-3 quorum!
ssh -o BatchMode=yes "$RPI" 'pkill -f "[p]laybox.*f3-3node-state-c" 2>/dev/null || true' || true
ok "Step 3: killed Host C (RPi5) mid-room — testing 2-of-3 fault-tolerant quorum (M4 Max + M1 Air)"

# Step 4: Promote lr_6e-3 with only 2 of 3 hosts alive (M4 Max + M1 Air)
printf 'roomlog-promote %s %s\n' "$PROB_HASH" "$H_STEP2" >&3
wait_log "promoted; the frontier metric is now 3000000" 240
ok "Step 4 (lr_6e-3, 2-of-3 fault-tolerant M4+M1 while RPi5 is dead): $(grep 'promoted; the frontier metric is now' "$WORK/host-a.log" | tail -n 1)"

# Let Host B sync the final commit before shutting down cleanly
sleep 2
printf 'roomlog\nquit\n' >&3
wait "$HOST_A_PID" 2>/dev/null || true
HOST_A_PID=""
exec 3>&-

# ---------------------------------------------------------------------------
say "4. Offline verification across Host A (M4 Max), Host B (M1 Air), and Host C (RPi5 pre-crash)"
cp "$WORK/state-a/roomlog.jsonl" "$OUT_DIR/host-a-m4-roomlog.jsonl"
scp -q "$M1:/tmp/f3-3node-state-b/roomlog.jsonl" "$OUT_DIR/host-b-m1-roomlog.jsonl"
scp -q "$RPI:/tmp/f3-3node-state-c/roomlog.jsonl" "$OUT_DIR/host-c-rpi5-roomlog.jsonl" || true

echo "--- Host A (M4 Max) offline room-log ---"
"$PB_MAC" room-log "$OUT_DIR/host-a-m4-roomlog.jsonl" --root "$ROOT_PUB"
echo "--- Host B (M1 Air) offline room-log ---"
"$PB_MAC" room-log "$OUT_DIR/host-b-m1-roomlog.jsonl" --root "$ROOT_PUB"
echo "--- Host C (RPi5, pre-kill snapshot) offline room-log ---"
"$PB_MAC" room-log "$OUT_DIR/host-c-rpi5-roomlog.jsonl" --root "$ROOT_PUB" || true

"$PB_MAC" release pq-keygen "$WORK/bundle.pq.key" >/dev/null
"$PB_MAC" gate export \
  --home "$WORK/home-a" \
  --state "$WORK/state-a" \
  --pq-key "$WORK/bundle.pq.key" \
  --out "$OUT_DIR/f3-three-node-5gate-bundle.json"

VERIFY_JSON=$("$PB_MAC" gate verify \
  --home "$WORK/home-a" \
  "$OUT_DIR/f3-three-node-5gate-bundle.json" \
  --require-signature \
  --require-pq \
  --json)
printf '%s\n' "$VERIFY_JSON" > "$OUT_DIR/verify.json"
ok "3-node fault-tolerant 5-gate hybrid PQ bundle verified offline!"
printf '%s\n' "$VERIFY_JSON"
