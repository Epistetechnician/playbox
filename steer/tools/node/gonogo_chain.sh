#!/bin/bash
# Go/no-go for the blind SwaT-style mapper on Qwen3-4B-Base -> Llama-3.2-3B. Run detached with setsid; marker line at the end.
export HF_HOME=~/tp/hf HF_HUB_CACHE=~/tp/hf/hub
~/tp/svc/bin/python -m pip install --quiet "datasets<4" > ~/tp/pip_datasets.log 2>&1  # into the venv, so it respects transformers' huggingface-hub pin
cd ~/tp/code/steer && tar xzf ~/gonogo_code.tgz
mkdir -p ~/tp/ckpt
echo "train start $(date -u +%T)" > ~/tp/gonogo.log
~/tp/svc/bin/python -c "import sys; sys.path.insert(0,'.'); from playbox_steer import l4_swat; import datasets" > ~/tp/gonogo_import.log 2>&1 || { echo "IMPORT FAILED" >> ~/tp/gonogo.log; echo "GONOGO_DONE $(date -u +%T)" >> ~/tp/gonogo.log; exit 1; }
~/tp/svc/bin/python -u tools/l4_train.py --sender Qwen/Qwen3-4B-Base --receiver unsloth/Llama-3.2-3B --out ~/tp/ckpt/q4l3_blind.pt --blind \
  --steps1 1500 --steps2 4000 --bs 32 --items 50000 --blocks 4 --width 768 --eval-every 500 --budget-s 4500 > ~/tp/gonogo_train.log 2>&1
echo "train exit $? $(date -u +%T)" >> ~/tp/gonogo.log
~/tp/svc/bin/python -u tools/l4_eval_cli.py ~/tp/ckpt/q4l3_blind.pt ~/tp/gonogo_eval.json > ~/tp/gonogo_eval.log 2>&1
echo "eval exit $? $(date -u +%T)" >> ~/tp/gonogo.log
echo "GONOGO_DONE $(date -u +%T)" >> ~/tp/gonogo.log
