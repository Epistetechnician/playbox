#!/bin/bash
# Go/no-go for the blind SwaT-style mapper on Qwen3-4B-Base -> Llama-3.2-3B. Run detached with setsid; marker line at the end.
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub PYTHONPATH=~/tp/dslib:$PYTHONPATH
cd ~/tp/code/steer && tar xzf ~/gonogo_code.tgz
mkdir -p ~/tp/ckpt
echo "train start $(date -u +%T)" > ~/tp/gonogo.log
~/tp/svc/bin/python -u tools/l4_train.py --sender Qwen/Qwen3-4B-Base --receiver unsloth/Llama-3.2-3B --out ~/tp/ckpt/q4l3_blind.pt --blind \
  --steps1 1500 --steps2 4000 --bs 32 --items 50000 --blocks 4 --width 768 --eval-every 500 --budget-s 4500 > ~/tp/gonogo_train.log 2>&1
echo "train exit $? $(date -u +%T)" >> ~/tp/gonogo.log
~/tp/svc/bin/python -u tools/l4_eval_cli.py ~/tp/ckpt/q4l3_blind.pt ~/tp/gonogo_eval.json > ~/tp/gonogo_eval.log 2>&1
echo "eval exit $? $(date -u +%T)" >> ~/tp/gonogo.log
echo "GONOGO_DONE $(date -u +%T)" >> ~/tp/gonogo.log
