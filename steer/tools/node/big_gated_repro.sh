#!/bin/bash
# Reproduction of the Qwen3-8B <-> OLMo-2-7B probe-gated run with a fresh seed label (s3), using the shipped probe
# (playbox_steer/telepathy_probe_big.json). Same protocol as docs/steering.md "Larger pair, gated": 20 topics x 2 reps x 2 directions
# plus the 36 held-out controls. Run on a bootstrapped gman node: gman run -d NODE -- bash ~/tp/code/steer/tools/node/big_gated_repro.sh
set -a; . ~/tp/tokens.env; set +a
export TELEPATHY_PAIR=qwen8b,olmo2 REPOS="qwen8b=Qwen/Qwen3-8B-Base,olmo2=allenai/OLMo-2-1124-7B" ALLOW_BIN=1
TOPICS=aviation,baseball,basketball,cars,chess,classical-music,cryptocurrency,dinosaurs,earthquake,jazz,law,medicine,ocean,photography,poker,politics,religion,soccer,space,volcano
cd ~/tp/code/steer
echo "stage stack $(date -u +%T)"
PROBE=$PWD/playbox_steer/telepathy_probe_big.json bash tools/node/stack.sh
echo "stage eval $(date -u +%T)"
TELEPATHY_GATED=1 TELEPATHY_REPS=2 TELEPATHY_WORKERS=6 TELEPATHY_CONTROLS_FILE=results/telepathy/controls_heldout.json \
  TELEPATHY_CONCEPTS=$TOPICS bash ~/tp/run_proto.sh big_pair_gated_s3
echo "REPRO_DONE $(date -u +%T)"
