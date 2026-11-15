#!/bin/bash
# Second corpus seed for the metered benchmark, small then big pair, same settings as the first run. Run after the seeds chain (the GPU must be otherwise idle).
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub
cd ~/tp/code/steer && tar xzf ~/metered_code.tgz
echo "m1 start $(date -u +%T)" > ~/tp/m1.log
for k in small big; do
  ~/tp/svc/bin/python -u tools/node/metered_run.py $k ~/tp/metered_${k}_seed1.json 40 1 > ~/tp/m1_$k.log 2>&1
  echo "pair $k exit $? $(date -u +%T)" >> ~/tp/m1.log
done
echo "M1_DONE $(date -u +%T)" >> ~/tp/m1.log
