#!/bin/bash
# One-time setup of the gman node: three isolated venvs (brain, telepathy service, Hermes) under ~/tp. Idempotent.
set -euo pipefail
mkdir -p ~/tp && cd ~/tp
export UV_LINK_MODE=copy
[ -x vllm/bin/vllm ] || { uv venv vllm --python 3.12 && VIRTUAL_ENV=~/tp/vllm uv pip install vllm==0.33.0 huggingface_hub; }
[ -x svc/bin/python ] || { uv venv svc --python 3.12 --system-site-packages \
  && VIRTUAL_ENV=~/tp/svc uv pip install "transformers>=4.51,<4.60" "numpy<2.2" blake3 safetensors huggingface_hub "fastapi[standard]" certifi; }
if [ ! -x hermes/bin/hermes ]; then  # Hermes refuses wheel builds: editable install of a clone, on python 3.14 (its pinned deps), from pypi.org
  [ -d hermes-src ] || git clone -q https://github.com/NousResearch/hermes-agent hermes-src
  (cd hermes-src && git checkout -q "${HERMES_REF:-67d4760}")
  uv venv hermes --python 3.14
  (cd hermes-src && unset UV_INDEX_URL UV_DEFAULT_INDEX && VIRTUAL_ENV=~/tp/hermes uv pip install --default-index https://pypi.org/simple -e .)
fi
~/tp/hermes/bin/hermes --version
echo SETUP_DONE
