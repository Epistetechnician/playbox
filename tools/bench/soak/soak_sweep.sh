#!/bin/sh
# Every SOAK_EVERY seconds: fresh checkout of origin/main, full workspace tests, one result line each. Needs an unsandboxed shell (netmon).
D=${SOAK_DIR:-$HOME/.playbox-soak/run}; EVERY=${SOAK_EVERY:-7200}; REPO=${REPO:-$HOME/Documents/GitHub/playbox}
mkdir -p "$D"
while :; do
  git -C "$REPO" fetch -q origin main
  W=$(mktemp -d); git -C "$REPO" worktree add -q --detach "$W" origin/main
  sha=$(git -C "$W" rev-parse --short HEAD); t0=$(date +%s)
  (cd "$W" && CARGO_TARGET_DIR=$HOME/.playbox-soak/target cargo test --workspace --no-fail-fast > "$D/sweep-$sha-$t0.log" 2>&1); rc=$?
  failed=$(grep -c "^test .* FAILED" "$D/sweep-$sha-$t0.log")
  echo "{\"at\":\"$(date +%H:%M:%S)\",\"sha\":\"$sha\",\"rc\":$rc,\"failed_tests\":$failed,\"secs\":$(( $(date +%s) - t0 ))}" >> "$D/sweep.jsonl"
  git -C "$REPO" worktree remove --force "$W"
  sleep "$EVERY"
done
