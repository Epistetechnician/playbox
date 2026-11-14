#!/bin/bash
# Re-fit the topic-gate probe for Qwen3-8B <-> OLMo-2-7B from FRESH live Agent A text, then run the gated eval with that new probe
# (not the shipped telepathy_probe_big.json). Same recipe as docs/steering.md "Larger pair, gated": 248 logged thoughts, grouped-CV
# ridge-logistic probe, then 20 topics x 2 reps x 2 directions plus the 36 held-out controls. Output label: big_pair_gated_fp (fresh probe).
# gman run -d NODE -- bash ~/tp/code/steer/tools/node/big_calib_repro.sh
set -a; . ~/tp/tokens.env; set +a
export TELEPATHY_PAIR=qwen8b,olmo2 REPOS="qwen8b=Qwen/Qwen3-8B-Base,olmo2=allenai/OLMo-2-1124-7B" ALLOW_BIN=1
TOPICS=aviation,baseball,basketball,cars,chess,classical-music,cryptocurrency,dinosaurs,earthquake,jazz,law,medicine,ocean,photography,poker,politics,religion,soccer,space,volcano
cd ~/tp/code/steer
echo "stage stack1 $(date -u +%T)"
LOG=~/tp/think_log_big_fp.jsonl bash tools/node/stack.sh
echo "stage calib $(date -u +%T)"
export HERMES_HOME=~/tp/hermes_home BRAIN_API_KEY=$BRAIN_TOKEN HERMES_BIN=~/tp/hermes/bin/hermes TELEPATHY_PY=python3 \
  TELEPATHY_REMOTE=1 TELEPATHY_AUTOSTART=0 TELEPATHY_URL=http://127.0.0.1:8765 TELEPATHY_WORKERS=8 TELEPATHY_OUT=results/telepathy/big_calib_fp
~/tp/svc/bin/python tools/telepathy_calib.py fit
echo "stage probe $(date -u +%T)"
(cd results/telepathy/big_calib_fp_private && ~/tp/svc/bin/python ../../../tools/telepathy_probe.py ~/tp/think_log_big_fp.jsonl calib_jobs.json ~/tp/probe_big_fp.json probe_report.json)
echo "stage stack2 $(date -u +%T)"
PROBE=~/tp/probe_big_fp.json bash tools/node/stack.sh
echo "stage eval $(date -u +%T)"
TELEPATHY_GATED=1 TELEPATHY_REPS=2 TELEPATHY_WORKERS=6 TELEPATHY_CONTROLS_FILE=results/telepathy/controls_heldout.json \
  TELEPATHY_CONCEPTS=$TOPICS bash ~/tp/run_proto.sh big_pair_gated_fp
echo "FP_DONE $(date -u +%T)"
