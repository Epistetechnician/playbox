#!/bin/zsh
# Start the Hermes confirmation only when the machine has room; otherwise give up after 3 hours.
free_gb() { vm_stat | awk '/Pages free|Pages inactive|Pages speculative/{n+=$NF+0} END{printf "%d", n*16384/2^30}'; }
swap_mb() { sysctl -n vm.swapusage | awk '{gsub("M","",$6); printf "%d", $6}'; }
for i in {1..180}; do
  if [ "$(free_gb)" -ge 14 ] && [ "$(swap_mb)" -lt 3000 ]; then
    echo "gate open: free $(free_gb) GB, swap $(swap_mb) MB"; break
  fi
  [ $i -eq 180 ] && { echo "gate never opened (free $(free_gb) GB, swap $(swap_mb) MB); not running"; exit 3; }
  sleep 60
done
cd "$(dirname $0)/../.."
TELEPATHY_URL=http://127.0.0.1:8766 TELEPATHY_MIN_FREE_GB=10 TELEPATHY_WORKERS=1 TELEPATHY_REPS=2 TELEPATHY_CONTROLS=4 \
TELEPATHY_CONCEPTS=ocean,space,jazz,classical-music,soccer,basketball,medicine,chess,poker,volcano,earthquake,dinosaurs,cars,aviation,cryptocurrency,politics,law,religion,photography,baseball \
HERMES_PROVIDER=anthropic HERMES_MODEL=claude-haiku-5-5 TELEPATHY_OUT=$PWD/steer/results/telepathy/confirm_big \
judge/.venv/bin/python steer/tools/telepathy_hermes.py 44
