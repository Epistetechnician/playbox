#!/usr/bin/env bash
# Boot command for the gman node: install deps into a venv that reuses the image's torch, run E8. See tools/e8_node.py.
set -euxo pipefail
cd ~/e8
python3 -m venv --system-site-packages venv
venv/bin/pip install -q "transformers>=4.51,<4.60" "numpy<2.2" blake3 safetensors huggingface_hub
venv/bin/python -c "import torch,transformers;print(torch.__version__,torch.cuda.is_available(),transformers.__version__)"
venv/bin/python tools/e8_node.py out/e8.json 2>&1 | tee out.log
