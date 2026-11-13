#!/bin/bash
# Qwen3-8B <-> DeepSeek-LLM-7B with DeepSeek steered at layer fraction 0.9, strength 1.0: stack (fits the new map once), then two gated evals.
echo "chain10 start $(date -u +%T)" >> ~/tp/chain.log
cd ~/tp/code/steer && tar xzf ~/recv_patch.tgz 2>/dev/null
set -a; . ~/tp/tokens.env; set +a
export TELEPATHY_COEFS=dsk=1.0 TELEPATHY_RECV_FRACS=dsk=0.9
export REPOS="qwen8b=Qwen/Qwen3-8B-Base,dsk=deepseek-ai/deepseek-llm-7b-base" ALLOW_BIN=1 PROBE=~/tp/probe_dsk.json
TOPICS=aviation,baseball,basketball,cars,chess,classical-music,cryptocurrency,dinosaurs,earthquake,jazz,law,medicine,ocean,photography,poker,politics,religion,soccer,space,volcano
bash tools/node/stack.sh > ~/tp/stack10.log 2>&1
echo "stack ready $(date -u +%T)" >> ~/tp/chain.log
export HERMES_HOME=~/tp/hermes_home BRAIN_API_KEY=$BRAIN_TOKEN HERMES_BIN=~/tp/hermes/bin/hermes TELEPATHY_PY=python3 TELEPATHY_REMOTE=1 TELEPATHY_AUTOSTART=0 TELEPATHY_URL=http://127.0.0.1:8765
for seed in 1 2; do
  echo "eval r09 s$seed start $(date -u +%T)" >> ~/tp/chain.log
  TELEPATHY_PAIR=qwen8b,dsk TELEPATHY_GATED=1 TELEPATHY_REPS=2 TELEPATHY_WORKERS=6 TELEPATHY_CONTROLS_FILE=results/telepathy/controls_heldout.json \
    TELEPATHY_CONCEPTS=$TOPICS bash ~/tp/run_proto.sh dsk_pair_r09_s$seed > ~/tp/eval_r09_s$seed.log 2>&1
done
echo "chain10 exit $(date -u +%T)" >> ~/tp/chain.log
