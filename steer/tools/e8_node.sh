#!/usr/bin/env bash
# Boot command for the gman node: unpack the uploaded source, install deps, run E8. See tools/e8_node.py.
set -euxo pipefail
cd ~/e8
pip install -q "transformers>=4.51,<4.60" "numpy<2.2" blake3 safetensors huggingface_hub
python tools/e8_node.py out/e8.json 2>&1 | tee out.log
