#!/usr/bin/env bash
# Run on the gman H100 node: train both directions, evaluate each, leave checkpoints and JSON under ~/l4/out.
#   cd ~/l4 && bash tools/l4_node.sh          (the launcher passes BUDGET_S, the per-direction training time cap in seconds)
set -euxo pipefail
cd ~/l4
python3 -m venv --system-site-packages venv
venv/bin/pip install -q "transformers>=4.51,<4.60" "numpy<2.2" blake3 safetensors huggingface_hub datasets
venv/bin/python -c "import torch,transformers;print(torch.__version__,torch.cuda.is_available(),transformers.__version__)"
mkdir -p out
SM=HuggingFaceTB/SmolLM2-135M
QW=Qwen/Qwen3-1.7B-Base
BUDGET_S=${BUDGET_S:-6000}
for dir in s2q q2s; do
  if [ "$dir" = s2q ]; then SND=$SM; RCV=$QW; else SND=$QW; RCV=$SM; fi
  venv/bin/python tools/l4_train.py --sender $SND --receiver $RCV --out out/$dir.pt --steps1 3000 --steps2 8000 --bs 48 --budget-s $BUDGET_S 2>&1 | tee out/train_$dir.log
  venv/bin/python tools/l4_eval_cli.py out/$dir.pt out/eval_$dir.json 2>&1 | tee out/eval_$dir.log
done
echo ALL_DONE
