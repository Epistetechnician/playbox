#!/bin/bash
# Qwen3-8B <-> Mistral-7B (a fourth family), one wake: layer sweep picks the receiver layer and strength, then stack, 248 live thoughts, probe, gated eval x2 seeds.
# Usage, on a bootstrapped gman node: gman run -d NODE -- bash ~/tp/code/steer/tools/node/mis_pipeline.sh
# Needs ~/tp/tokens.env (defines BRAIN_TOKEN) and the node layout from bootstrap.sh. Prints "PIPELINE_DONE" when every stage finished.
set -euo pipefail
set -a; . ~/tp/tokens.env; set +a
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub
cd ~/tp/code/steer
R="qwen8b=Qwen/Qwen3-8B-Base,mis=mistralai/Mistral-7B-v0.1"
TOPICS=aviation,baseball,basketball,cars,chess,classical-music,cryptocurrency,dinosaurs,earthquake,jazz,law,medicine,ocean,photography,poker,politics,religion,soccer,space,volcano
pkill -f telepathy_node.py || true; pkill -f "vllm serve" || true; sleep 5
FR=0.9; CO=1.4  # chosen by the sweep (mis_sweep.json), which finished before the node stopped
[ "$FR" = "0.45" ] && RF="" || RF="mis=$FR"
export TELEPATHY_COEFS="mis=$CO" ${RF:+TELEPATHY_RECV_FRACS=$RF}
export TELEPATHY_PAIR=qwen8b,mis REPOS="$R"
echo "stage stack1 $(date -u +%T)"
LOG=~/tp/think_log_mis.jsonl bash tools/node/stack.sh
echo "stage calib $(date -u +%T)"
export HERMES_HOME=~/tp/hermes_home BRAIN_API_KEY=$BRAIN_TOKEN HERMES_BIN=~/tp/hermes/bin/hermes TELEPATHY_PY=python3 \
  TELEPATHY_REMOTE=1 TELEPATHY_AUTOSTART=0 TELEPATHY_URL=http://127.0.0.1:8765 TELEPATHY_WORKERS=8 TELEPATHY_OUT=results/telepathy/mis_calib
~/tp/svc/bin/python tools/telepathy_calib.py fit
echo "stage probe $(date -u +%T)"
(cd results/telepathy/mis_calib_private && ~/tp/svc/bin/python ../../../tools/telepathy_probe.py ~/tp/think_log_mis.jsonl calib_jobs.json ~/tp/probe_mis.json probe_report.json)
echo "stage stack2 $(date -u +%T)"
PROBE=~/tp/probe_mis.json bash tools/node/stack.sh
for seed in 1 2; do
  echo "stage eval$seed $(date -u +%T)"
  TELEPATHY_PAIR=qwen8b,mis TELEPATHY_GATED=1 TELEPATHY_REPS=2 TELEPATHY_WORKERS=6 TELEPATHY_CONTROLS_FILE=results/telepathy/controls_heldout.json \
    TELEPATHY_CONCEPTS=$TOPICS bash ~/tp/run_proto.sh mis_pair_gated_s$seed
done
echo "PIPELINE_DONE $(date -u +%T)"
