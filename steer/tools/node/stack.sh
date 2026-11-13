#!/bin/bash
# Telepathy service (two models, bf16) + the 27B brain on one H100. Env: REPOS="a=repo,b=repo" [PROBE=file.json] [LOG=file.jsonl] [ALLOW_BIN=1]
export HF_HOME=~/tp/hf CUDA_HOME=/usr/local/cuda VLLM_USE_DEEP_GEMM=0 PATH=/usr/local/cuda/bin:$PATH
set -a; . ~/tp/tokens.env; set +a
pkill -f telepathy_node.py; pkill -f "vllm serve"; sleep 8
cd ~/tp/code/steer
[ -n "${PROBE:-}" ] && export TELEPATHY_PROBE=$PROBE
[ -n "${ALLOW_BIN:-}" ] && export TELEPATHY_ALLOW_BIN=1
: > ~/tp/svc.log
TELEPATHY_LOG=${LOG:-} TELEPATHY_REPOS="$REPOS" TELEPATHY_DTYPE=bfloat16 TELEPATHY_FIT=agent TELEPATHY_MAPS=~/tp/maps \
  nohup ~/tp/svc/bin/python tools/telepathy_node.py > ~/tp/svc.log 2>&1 &
until grep -aq "ready" ~/tp/svc.log || ! kill -0 $! 2>/dev/null; do sleep 5; done
nohup ~/tp/vllm/bin/vllm serve Qwen/Qwen3.8-27B-FP8 --served-model-name qwen3.8-27b --host 127.0.0.1 --port 8000 \
  --api-key "$BRAIN_TOKEN" --max-model-len 65536 --gpu-memory-utilization 0.5 --enable-auto-tool-choice \
  --tool-call-parser qwen3_coder --reasoning-parser qwen3 --enable-prefix-caching --max-num-seqs 32 > ~/tp/brain.log 2>&1 &
until curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:8000/health | grep -q 200; do sleep 5; done
echo STACK_READY
