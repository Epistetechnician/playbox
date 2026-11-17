#!/bin/bash
# v2 recipe vs old recipe, blind Qwen3-4B-Base -> Llama-3.2-3B. A 40-step smoke test first; the chain stops if it fails.
# Order interleaves the recipes so a partial run is still informative. Logs removed first so a stale marker cannot end a watch early.
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub
cd ~/tp/code/steer && tar xzf ~/v2_code.tgz
rm -f ~/tp/v2.log ~/tp/v2_*.json ~/tp/v2_*.log; P=~/tp/svc/bin/python
SND=Qwen/Qwen3-4B-Base; RCV=unsloth/Llama-3.2-3B
echo "smoke start $(date -u +%T)" > ~/tp/v2.log
$P -u tools/l4_train_v2.py --sender $SND --receiver $RCV --out ~/tp/ckpt/v2_smoke.pt --steps1 30 --steps2 30 --bs 8 --items 2000 --eval-every 30 --ridge-batches 20 > ~/tp/v2_smoke.log 2>&1
rc=$?; echo "smoke exit $rc $(date -u +%T)" >> ~/tp/v2.log
if [ $rc -ne 0 ] || ! grep -aq '"mapped"' ~/tp/v2_smoke.log || grep -aq "mapped\": NaN" ~/tp/v2_smoke.log; then echo "SMOKE FAILED" >> ~/tp/v2.log; echo "V2_DONE $(date -u +%T)" >> ~/tp/v2.log; exit 1; fi
rm -f ~/tp/ckpt/v2_smoke.pt
run_v2() { sd=$1
  echo "v2 s$sd train start $(date -u +%T)" >> ~/tp/v2.log
  $P -u tools/l4_train_v2.py --sender $SND --receiver $RCV --out ~/tp/ckpt/v2_s$sd.pt --seed $sd --budget-s 4500 > ~/tp/v2_train_s$sd.log 2>&1
  echo "v2 s$sd train exit $? $(date -u +%T)" >> ~/tp/v2.log
  $P -u tools/l4_eval_b_cli.py ~/tp/ckpt/v2_s$sd.pt ~/tp/v2_evalb_s$sd.json > ~/tp/v2_evalb_s$sd.log 2>&1
  echo "v2 s$sd eval exit $? $(date -u +%T)" >> ~/tp/v2.log
  rm -f ~/tp/ckpt/v2_s$sd.pt; }
run_v1() { sd=$1
  echo "v1 s$sd train start $(date -u +%T)" >> ~/tp/v2.log
  $P -u tools/l4_train.py --sender $SND --receiver $RCV --out ~/tp/ckpt/q4l3_blind_s$sd.pt --blind --seed $sd --steps1 1500 --steps2 4000 --bs 32 --items 50000 --blocks 4 --width 768 --eval-every 500 --budget-s 3600 > ~/tp/v2_train_v1_s$sd.log 2>&1
  echo "v1 s$sd train exit $? $(date -u +%T)" >> ~/tp/v2.log
  $P -u tools/l4_eval_b_cli.py ~/tp/ckpt/q4l3_blind_s$sd.pt ~/tp/v2_evalb_v1_s$sd.json > ~/tp/v2_evalb_v1_s$sd.log 2>&1
  echo "v1 s$sd eval exit $? $(date -u +%T)" >> ~/tp/v2.log; }
run_v2 0; run_v2 1; run_v1 2; run_v2 2; run_v1 3; run_v2 3
echo "V2_DONE $(date -u +%T)" >> ~/tp/v2.log
