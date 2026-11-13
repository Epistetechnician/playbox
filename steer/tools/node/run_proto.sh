#!/bin/bash
# Two-agent telepathy protocol on the node. Usage: run_proto.sh <out-name> (concepts etc. from the environment)
set -e
OUT=$1; shift
cd ~/tp/code/steer
export HERMES_HOME=~/tp/hermes_home BRAIN_API_KEY="${BRAIN_API_KEY:-$BRAIN_TOKEN}" HERMES_BIN=~/tp/hermes/bin/hermes TELEPATHY_PY=python3 \
  TELEPATHY_REMOTE=1 TELEPATHY_AUTOSTART=0 TELEPATHY_URL=http://127.0.0.1:8765 TELEPATHY_OUT=results/telepathy/$OUT
exec ~/tp/svc/bin/python tools/telepathy_hermes.py
