#!/usr/bin/env bash
# One text-blind mapper on the gman node: train then evaluate.  bash tools/l4_one.sh s2q|q2s   (run from ~/l4b)
set -euxo pipefail
cd ~/l4b
mkdir -p out
SM=HuggingFaceTB/SmolLM2-135M
QW=Qwen/Qwen3-1.7B-Base
PY=~/l4/venv/bin/python
dir=$1
if [ "$dir" = s2q ]; then SND=$SM; RCV=$QW; else SND=$QW; RCV=$SM; fi
$PY tools/l4_train.py --sender $SND --receiver $RCV --out out/${dir}_blind.pt --steps1 2500 --steps2 7000 --bs 48 --budget-s ${BUDGET_S:-4200} --blind --templ-frac 0.5 2>&1 | tee out/train_${dir}_blind.log
$PY tools/l4_eval_cli.py out/${dir}_blind.pt out/eval_${dir}_blind.json 2>&1 | tee out/eval_${dir}_blind.log
echo DONE_$dir
