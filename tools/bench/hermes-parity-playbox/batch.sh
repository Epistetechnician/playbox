#!/bin/sh
# Alternate P and L, one at a time (so wall-clock is not distorted by overlap). Usage: batch.sh N
cd "$(dirname "$0")/../../.."
for i in $(seq 1 "$1"); do
  python3 tools/bench/hermes-parity-playbox/run.py P "$i" --tag main
  python3 tools/bench/hermes-parity-playbox/run.py L "$i" --tag main
done
