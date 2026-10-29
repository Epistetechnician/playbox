#!/bin/sh
# End-to-end demo. Order: cheat (memo), lossy (bf16), real win (kv_cache), noise-level tweak, duplicate.
cd "$(dirname "$0")"
PY=../steer/.venv/bin/python
rm -rf state
$PY gate.py a tier_a_matmul/candidates/cheat_rank6.json --author mallory
$PY gate.py a tier_a_matmul/candidates/strassen.json --author alice
$PY gate.py b tier_b_lm/candidates/cheat_memo.py --author mallory
$PY gate.py b tier_b_lm/candidates/bf16.py --author carol
$PY gate.py b tier_b_lm/candidates/kv_cache.py --author alice
$PY gate.py b tier_b_lm/candidates/kv_cache_threads.py --author bob
$PY gate.py credit
$PY gate.py verify
