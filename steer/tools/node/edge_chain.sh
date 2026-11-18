#!/bin/bash
# Edge controls: the old recipe's checkpoints (seeds 0-3, still on the node) scored with the same 4 edge tokens, plus two controls
# (unrelated real cache + edge tokens; edge tokens with no mapped cache). Writes ~/tp/edge_*.json, marker EDGE_DONE in ~/tp/edge.log.
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub
cd ~/tp/code/steer && tar xzf ~/edge_code.tgz && grep -q edge_k playbox_steer/l4_eval_b.py || { echo "EDGE_DONE bad code" > ~/tp/edge.log; exit 1; }
rm -f ~/tp/edge.log ~/tp/edge_*.json; P=~/tp/svc/bin/python
for sd in 0 1 2 3; do
  ck=~/tp/ckpt/q4l3_blind_s$sd.pt; [ $sd = 0 ] && ck=~/tp/ckpt/q4l3_blind.pt
  echo "v1 s$sd edge start $(date -u +%T)" >> ~/tp/edge.log
  $P -u tools/l4_eval_b_cli.py $ck ~/tp/edge_v1_s$sd.json 4 > ~/tp/edge_v1_s$sd.log 2>&1
  echo "v1 s$sd edge exit $? $(date -u +%T)" >> ~/tp/edge.log
done
echo "EDGE_DONE $(date -u +%T)" >> ~/tp/edge.log
