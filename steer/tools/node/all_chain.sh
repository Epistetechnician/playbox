#!/bin/bash
# Metered benchmark (fixed decode), then the blind-mapper go/no-go. Old logs are removed first so a stale marker cannot end a watch early.
rm -f ~/tp/metered.log ~/tp/gonogo.log ~/tp/all.log ~/tp/gonogo_eval.json
bash ~/tp/metered_chain.sh
bash ~/tp/gonogo_chain.sh
echo "ALL_CHAIN_DONE $(date -u +%T)" >> ~/tp/all.log
