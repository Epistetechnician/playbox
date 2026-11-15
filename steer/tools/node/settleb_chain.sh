#!/bin/bash
# Settle (B): evaluate the seed-0 mapper on many templated draws, train a seed-1 mapper, evaluate it the same way. Old logs removed first.
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub
cd ~/tp/code/steer && tar xzf ~/settleb_code.tgz
rm -f ~/tp/settleb.log ~/tp/settleb_*.json
echo "eval s0 start $(date -u +%T)" > ~/tp/settleb.log
~/tp/svc/bin/python -u tools/l4_eval_b_cli.py ~/tp/ckpt/q4l3_blind.pt ~/tp/settleb_s0.json > ~/tp/settleb_eval_s0.log 2>&1
echo "eval s0 exit $? $(date -u +%T)" >> ~/tp/settleb.log
~/tp/svc/bin/python -u tools/l4_train.py --sender Qwen/Qwen3-4B-Base --receiver unsloth/Llama-3.2-3B --out ~/tp/ckpt/q4l3_blind_s1.pt --blind --seed 1 \
  --steps1 1500 --steps2 4000 --bs 32 --items 50000 --blocks 4 --width 768 --eval-every 500 --budget-s 4500 > ~/tp/settleb_train_s1.log 2>&1
echo "train s1 exit $? $(date -u +%T)" >> ~/tp/settleb.log
~/tp/svc/bin/python -u tools/l4_eval_b_cli.py ~/tp/ckpt/q4l3_blind_s1.pt ~/tp/settleb_s1.json > ~/tp/settleb_eval_s1.log 2>&1
echo "eval s1 exit $? $(date -u +%T)" >> ~/tp/settleb.log
echo "SETTLEB_DONE $(date -u +%T)" >> ~/tp/settleb.log
