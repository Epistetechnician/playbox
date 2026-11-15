#!/usr/bin/env bash
# Local-only version history for raw experiment artifacts (steer/results/**/*.{prompt,out,pt}).
# The main repo ignores them; this keeps them under git in .git/artifacts.git, which `git push`
# never sends. Usage: scripts/artifacts.sh [init|snap [msg]|status|log|verify]
set -euo pipefail
root=$(git rev-parse --show-toplevel)
gd="$root/.git/artifacts.git"
a() { git --git-dir="$gd" --work-tree="$root/steer/results" "$@"; }
raw=(':(glob)**/*.prompt' ':(glob)**/*.out' ':(glob)**/*.pt')
case "${1:-status}" in
  init)
    [ -d "$gd" ] || git init -q --bare "$gd"
    a config user.name "playbox-artifacts"; a config user.email "artifacts@localhost"
    a config core.excludesFile /dev/null; a config status.showUntrackedFiles all
    echo "artifact repo at $gd" ;;
  snap)
    a add -A -- "${raw[@]}"
    if a diff --cached --quiet; then echo "nothing new"; else a commit -q -m "${2:-snapshot $(date -u +%FT%TZ)}"; a log --oneline -1; fi ;;
  status) a status --short -- "${raw[@]}" | head -40 ;;
  log) a log --oneline | head -20 ;;
  verify) a diff --quiet -- "${raw[@]}" && echo "artifacts match last snapshot" || { echo "artifacts differ from last snapshot"; exit 1; } ;;
  *) echo "usage: $0 [init|snap [msg]|status|log|verify]" >&2; exit 2 ;;
esac
