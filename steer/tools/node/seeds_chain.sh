#!/bin/bash
# More training seeds for the blind Qwen3-4B -> Llama-3.2-3B mapper: (A)/(C) for seed 1, then seeds 2 and 3 (train, (B) eval, (A)/(C) eval).
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub
cd ~/tp/code/steer && tar xzf ~/settleb_code.tgz
rm -f ~/tp/seeds.log; P=~/tp/svc/bin/python
echo "ac s1 start $(date -u +%T)" > ~/tp/seeds.log
$P -u tools/l4_eval_cli.py ~/tp/ckpt/q4l3_blind_s1.pt ~/tp/ac_s1.json > ~/tp/ac_s1.log 2>&1
for sd in 2 3; do
  echo "train s$sd start $(date -u +%T)" >> ~/tp/seeds.log
  $P -u tools/l4_train.py --sender Qwen/Qwen3-4B-Base --receiver unsloth/Llama-3.2-3B --out ~/tp/ckpt/q4l3_blind_s$sd.pt --blind --seed $sd \
    --steps1 1500 --steps2 4000 --bs 32 --items 50000 --blocks 4 --width 768 --eval-every 500 --budget-s 3000 > ~/tp/train_s$sd.log 2>&1
  echo "train s$sd exit $? $(date -u +%T)" >> ~/tp/seeds.log
  $P -u tools/l4_eval_b_cli.py ~/tp/ckpt/q4l3_blind_s$sd.pt ~/tp/settleb_s$sd.json > ~/tp/evalb_s$sd.log 2>&1
  $P -u tools/l4_eval_cli.py ~/tp/ckpt/q4l3_blind_s$sd.pt ~/tp/ac_s$sd.json > ~/tp/ac_s$sd.log 2>&1
  echo "eval s$sd exit $? $(date -u +%T)" >> ~/tp/seeds.log
done
echo "SEEDS_DONE $(date -u +%T)" >> ~/tp/seeds.log
