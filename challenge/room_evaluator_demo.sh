#!/bin/sh
# A stand-in evaluator for `playbox up --room-log-evaluator`, to try promotion-as-a-quorum-rule on any machine
# (docs/host-less-rooms.md, M5). It is called with the problem hash and the candidate hash and prints one JSON line.
#
# The metric is deterministic in the candidate (so two honest hosts agree without timing noise); the environment
# hash is this machine's kernel and architecture, so receipts from different machines say so; the tier comes from
# $PB_TIER (0 when unset). A real evaluator measures the candidate in a sandbox and reports what it measured.
set -eu
problem=$1
candidate=$2
n=$(printf '%d' "0x$(printf %s "$candidate" | cut -c1-4)")
metric=$((1100000 + n))
env_hash=$(uname -srm | python3 -c 'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())')
weights=$(printf 'demo-weights-v1' | python3 -c 'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())')
printf '{"metric":%s,"weights":"%s","env":"%s","tier":%s}\n' "$metric" "$weights" "$env_hash" "${PB_TIER:-0}"
