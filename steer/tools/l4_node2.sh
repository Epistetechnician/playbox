#!/usr/bin/env bash
# Second node chain: text-blind mappers for both directions (no receiver token identities), then the sighted Qwen3->SmolLM2 mapper.
# Run from ~/l4b on the gman node; reuses the venv of ~/l4. Outputs under ~/l4b/out.
set -euxo pipefail
cd ~/l4b
mkdir -p out
SM=HuggingFaceTB/SmolLM2-135M
QW=Qwen/Qwen3-1.7B-Base
PY=~/l4/venv/bin/python
for spec in "s2q blind" "q2s blind" "q2s sighted"; do
  set -- $spec
  dir=$1; mode=$2
  if [ "$dir" = s2q ]; then SND=$SM; RCV=$QW; else SND=$QW; RCV=$SM; fi
  FLAG=""; [ "$mode" = blind ] && FLAG="--blind"
  $PY tools/l4_train.py --sender $SND --receiver $RCV --out out/${dir}_$mode.pt --steps1 2500 --steps2 7000 --bs 48 --budget-s ${BUDGET_S:-3300} $FLAG 2>&1 | tee out/train_${dir}_$mode.log
  $PY tools/l4_eval_cli.py out/${dir}_$mode.pt out/eval_${dir}_$mode.json 2>&1 | tee out/eval_${dir}_$mode.log
done
echo ALL_DONE
