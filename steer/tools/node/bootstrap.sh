#!/bin/bash
# Fresh gman H100 node -> everything the telepathy runs need, under ~/tp. Run once after unpacking steer_code.tgz into ~/tp/code/steer.
#   gman cp steer_code.tgz NODE:/home/dev/steer_code.tgz ; gman cp steer_ctl.tgz NODE:/home/dev/steer_ctl.tgz ; gman run -d NODE -- bash /home/dev/bootstrap.sh
set -euo pipefail
mkdir -p ~/tp/code/steer && cd ~/tp/code/steer && tar xzf ~/steer_code.tgz 2>/dev/null && tar xzf ~/steer_ctl.tgz 2>/dev/null
command -v uv >/dev/null || pip install -q uv
bash ~/tp/code/steer/tools/node_setup.sh
# a Hermes home whose brain is the vLLM server on loopback (stack.sh starts it); approvals off so Agent A/B may run their one command
mkdir -p ~/tp/hermes_home
cat > ~/tp/hermes_home/config.yaml <<'Y'
model:
  default: qwen3.8-27b
  provider: custom
  base_url: http://127.0.0.1:8000/v1
  key: BRAIN_KEY_PLACEHOLDER
approvals:
  mode: "off"
  cron_mode: deny
Y
[ -f ~/tp/tokens.env ] || { echo "BRAIN_TOKEN=$(head -c 24 /dev/urandom | base64 | tr -dc A-Za-z0-9)" > ~/tp/tokens.env
  echo "TELEPATHY_TOKEN=$(head -c 24 /dev/urandom | base64 | tr -dc A-Za-z0-9)" >> ~/tp/tokens.env; chmod 600 ~/tp/tokens.env; }
set -a; . ~/tp/tokens.env; set +a
sed -i "s/BRAIN_KEY_PLACEHOLDER/$BRAIN_TOKEN/" ~/tp/hermes_home/config.yaml
chmod 600 ~/tp/hermes_home/config.yaml
cp ~/tp/code/steer/tools/node/run_proto.sh ~/tp/run_proto.sh
echo BOOTSTRAP_DONE
