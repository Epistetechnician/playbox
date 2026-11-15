#!/bin/bash
# Metered benchmark, small pair then big pair, one process each; ends with a marker line. Run detached with setsid.
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub
cd ~/tp/code/steer && tar xzf ~/metered_code.tgz
pkill -f telepathy_node.py; pkill -f "vllm serve"; sleep 5
echo "chain start $(date -u +%T)" > ~/tp/metered.log
for k in small big; do
  echo "pair $k start $(date -u +%T)" >> ~/tp/metered.log
  ~/tp/svc/bin/python -u tools/node/metered_run.py $k ~/tp/metered_$k.json 40 > ~/tp/metered_$k.log 2>&1
  echo "pair $k exit $? $(date -u +%T)" >> ~/tp/metered.log
done
echo "CHAIN_DONE $(date -u +%T)" >> ~/tp/metered.log
