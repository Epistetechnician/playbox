#!/bin/bash
# Qwen3-8B <-> DeepSeek-LLM-7B: stack with a logging node, 248 live Agent A thoughts, probe fit, restart gated, gated eval x2 seeds.
set -a; . ~/tp/tokens.env; set +a
export TELEPATHY_PAIR=qwen8b,dsk REPOS="qwen8b=Qwen/Qwen3-8B-Base,dsk=deepseek-ai/deepseek-llm-7b-base" ALLOW_BIN=1
TOPICS=aviation,baseball,basketball,cars,chess,classical-music,cryptocurrency,dinosaurs,earthquake,jazz,law,medicine,ocean,photography,poker,politics,religion,soccer,space,volcano
cd ~/tp/code/steer
echo "stage stack1 $(date -u +%T)"
LOG=~/tp/think_log_dsk.jsonl bash tools/node/stack.sh
echo "stage calib $(date -u +%T)"
export HERMES_HOME=~/tp/hermes_home BRAIN_API_KEY=$BRAIN_TOKEN HERMES_BIN=~/tp/hermes/bin/hermes TELEPATHY_PY=python3 \
  TELEPATHY_REMOTE=1 TELEPATHY_AUTOSTART=0 TELEPATHY_URL=http://127.0.0.1:8765 TELEPATHY_WORKERS=8 TELEPATHY_OUT=results/telepathy/dsk_calib
~/tp/svc/bin/python tools/telepathy_calib.py fit
echo "stage probe $(date -u +%T)"
(cd results/telepathy/dsk_calib_private && ~/tp/svc/bin/python ../../../tools/telepathy_probe.py ~/tp/think_log_dsk.jsonl calib_jobs.json ~/tp/probe_dsk.json probe_report.json)
echo "stage stack2 $(date -u +%T)"
PROBE=~/tp/probe_dsk.json bash tools/node/stack.sh
for seed in 1 2; do
  echo "stage eval$seed $(date -u +%T)"
  TELEPATHY_PAIR=qwen8b,dsk TELEPATHY_GATED=1 TELEPATHY_REPS=2 TELEPATHY_WORKERS=6 TELEPATHY_CONTROLS_FILE=results/telepathy/controls_heldout.json \
    TELEPATHY_CONCEPTS=$TOPICS bash ~/tp/run_proto.sh dsk_pair_gated_s$seed
done
echo "PIPELINE_DONE $(date -u +%T)"
