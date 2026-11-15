#!/bin/sh
# The Playbox demo, one story: strangers' agents work on a shared score, and nobody has to trust anybody.
#
#   scripts/demo.sh            # about a minute after the first build
#   scripts/demo.sh --replay   # also re-runs the four promoted candidates on this machine (needs torch, about 3 min on CPU)
#
# Needs: git, a Rust toolchain (or PLAYBOX_BIN set to a built `playbox`), Docker running, python3 with `cryptography` and `numpy`.
# Everything it writes goes to a temporary directory that is removed at the end. See docs/demo.md for what each step shows.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here"

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
ok() { printf '   ok: %s\n' "$*"; }
die() { printf '   FAILED: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is not installed ($2)"; }

need git "to make the room's repository"
need python3 "to verify the hill-climb receipts"
python3 -c 'import cryptography, numpy' 2>/dev/null || die "python3 needs the 'cryptography' and 'numpy' modules (pip install cryptography numpy, or apt install python3-cryptography python3-numpy)"
docker info >/dev/null 2>&1 || die "Docker is not running (tasks run in a container; start Docker, or see docs/libkrun.md for the microVM runner)"

PB=${PLAYBOX_BIN:-}
if [ -z "$PB" ]; then
    need cargo "to build playbox, or set PLAYBOX_BIN"
    say "Building playbox from this checkout"
    cargo build --quiet -p playbox-cli
    PB="$here/target/debug/playbox"
fi
"$PB" --version

work=$(mktemp -d)
host_pid=""
cleanup() {
    [ -n "$host_pid" ] && kill "$host_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------------------------------------------
say "1. A host opens a room on a repository"
git init -q -b main "$work/repo"
(
    cd "$work/repo"
    git config user.email host@example.invalid
    git config user.name host
    echo "score: 1" > score.txt
    git add score.txt
    git commit -qm "the starting point"
)
"$PB" up --local --home "$work/host" --state "$work/state" --repo "$work/repo" --docker > "$work/up.log" 2>&1 &
host_pid=$!
i=0
until grep -q '^playbox://' "$work/up.log"; do
    i=$((i + 1))
    [ "$i" -gt 120 ] && { cat "$work/up.log"; die "the host did not start"; }
    kill -0 "$host_pid" 2>/dev/null || { cat "$work/up.log"; die "the host stopped"; }
    sleep 0.5
done
link=$(grep -o '^playbox://[^ ]*' "$work/up.log" | head -1)
ok "room is up; the link is $(printf %s "$link" | wc -c | tr -d ' ') characters: a network path plus a single-use signed invite"

# ---------------------------------------------------------------------------------------------------------------
say "2. A stranger joins by the link and runs a task. It runs in a sandbox, not on the host"
# The guest has its own key and its own home. It knows nothing about the host but the link.
task='echo "score: 2" > score.txt; echo "uid=$(id -u)"; wget -q -T 3 -O /dev/null http://1.1.1.1/ && echo NETWORK || echo "no network"'
"$PB" join "$link" --local --home "$work/guest" --task "$task" > "$work/task.log" 2>&1 || { cat "$work/task.log"; die "the task did not run"; }
sed 's/^/   | /' "$work/task.log"
grep -q "no network" "$work/task.log" || die "the task had a network"
grep -q '^+score: 2' "$work/task.log" || die "the task's change did not come back as a patch"
[ "$(cat "$work/repo/score.txt")" = "score: 1" ] || die "the task changed the host's checkout"
ok "the change came back as a patch; the host's own files are untouched; the task had no network"

# ---------------------------------------------------------------------------------------------------------------
say "3. The invite was single-use: the same link does not admit a second stranger"
if "$PB" join "$link" --local --home "$work/guest2" > "$work/join2.log" 2>&1; then
    cat "$work/join2.log"
    die "a spent invite was accepted"
fi
ok "refused: $(tail -1 "$work/join2.log" | cut -c1-110)"

# ---------------------------------------------------------------------------------------------------------------
say "4. Everything the host did is in a signed, hash-chained log. Edit one byte and it stops verifying"
kill "$host_pid" 2>/dev/null || true
wait "$host_pid" 2>/dev/null || true
host_pid=""
"$PB" audit "$work/state/audit.log" > "$work/audit.txt" 2>&1 || { cat "$work/audit.txt"; die "the audit log did not verify"; }
sed 's/^/   | /' "$work/audit.txt" | cut -c1-150
python3 - "$work/state/audit.log" "$work/forged.log" <<'PY'
import sys
text = open(sys.argv[1]).read()
assert '"exit_code":0' in text, "no task entry to forge"
open(sys.argv[2], "w").write(text.replace('"exit_code":0', '"exit_code":7', 1))
PY
if "$PB" audit "$work/forged.log" > "$work/forged.txt" 2>&1; then
    die "an edited audit log verified"
fi
ok "the edited copy is rejected: $(grep -v '^$' "$work/forged.txt" | tail -1 | cut -c1-110)"

# ---------------------------------------------------------------------------------------------------------------
say "5. A room that climbs a score: every promotion is a signed receipt anyone can re-derive"
run=docs/f3/dry_run
python3 challenge/f3/verify.py --work "$run" --host-pubkey "$(cat "$run/host.pub")" > "$work/verify.txt" 2>&1 || true
grep -E '^\[FAIL\]|^RESULT' "$work/verify.txt" | sed 's/^/   | /'
grep -q '^RESULT: VERIFIED' "$work/verify.txt" || { cat "$work/verify.txt"; die "the recorded run did not verify"; }
grep -q '"caught_false_promotions": 1' "$work/verify.txt" || die "the injected cheat is not recorded as caught"
ok "$(grep -c '^\[PASS\]' "$work/verify.txt") checks pass from the receipts, the archived candidates and this repository alone"
ok "4 candidates promoted; 1 that memorised the visible dev set beat the threshold there and was rejected on the sealed holdout"

# ---------------------------------------------------------------------------------------------------------------
say "6. The host cannot quietly promote the cheat either"
cp -R "$run" "$work/forged_run"
python3 - "$work/forged_run/receipts.jsonl" <<'PY'
import sys
path = sys.argv[1]
lines = open(path).read().splitlines()
for i, line in enumerate(lines):
    if '"holdout_failed"' in line:
        lines[i] = line.replace('"reject"', '"promote"', 1)
        break
else:
    raise SystemExit("no holdout_failed receipt to forge")
open(path, "w").write("\n".join(lines) + "\n")
PY
python3 challenge/f3/verify.py --work "$work/forged_run" --host-pubkey "$(cat "$run/host.pub")" > "$work/forged_verify.txt" 2>&1 || true
grep -q '^RESULT: VERIFIED' "$work/forged_verify.txt" && die "a forged promotion verified"
ok "rejected: $(grep '^\[FAIL\]' "$work/forged_verify.txt" | head -1 | cut -c8-120)"

if [ "${1:-}" = "--replay" ]; then
    say "7. Replay: train each promoted candidate again on this machine, on seeds the candidates never saw"
    python3 -c 'import torch' 2>/dev/null || die "the replay needs torch (pip install torch)"
    python3 challenge/f3/verify.py --work "$run" --host-pubkey "$(cat "$run/host.pub")" --replay all > "$work/replay.txt" 2>&1 || true
    grep -E 'replay|^RESULT' "$work/replay.txt" | sed 's/^/   | /' | cut -c1-170
    grep -q '^RESULT: VERIFIED' "$work/replay.txt" || die "the replay did not verify"
    ok "every promotion reproduces here, and the cheat fails the holdout again"
fi

say "Done. Nothing above asked you to trust the host, the guest, or this script's author:"
echo "   the grant bounded the guest, the sandbox held the task, the log and the receipts are signed and re-checkable."
echo "   What this does not show is in docs/demo.md."
