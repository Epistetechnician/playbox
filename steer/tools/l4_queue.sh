#!/usr/bin/env bash
# Wait for the sighted Qwen3->SmolLM2 job (writes ~/l4/out/eval_q2s.json) so GPU memory is free, then run the two text-blind
# mappers one after the other. Exits non-zero if either fails, so a watcher sees the failure.   (run from ~/l4b)
set -uo pipefail
cd ~/l4b
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True
while [ ! -f ~/l4/out/eval_q2s.json ]; do
  pgrep -f "l4_train.py|l4_eval_cli.py" >/dev/null || { echo "sighted job is gone without eval_q2s.json"; break; }
  sleep 20
done
bash tools/l4_one.sh s2q > run_s2q.log 2>&1 || { echo "FAIL s2q"; exit 1; }
bash tools/l4_one.sh q2s > run_q2s.log 2>&1 || { echo "FAIL q2s"; exit 1; }
echo QUEUE_DONE
