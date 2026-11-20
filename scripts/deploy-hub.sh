#!/usr/bin/env bash
# Atomic deploy of Playbox Hub UI + backend (+ optional release binaries) to ~/.playbox/hub-app/current
# and post-deploy smoke verification. Never touches running room hosts.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HUB_CUR="${PLAYBOX_HUB_APP:-$HOME/.playbox/hub-app/current}"
WITH_BINARIES=0
for arg in "$@"; do
  case "$arg" in
    --binaries) WITH_BINARIES=1 ;;
  esac
done

echo "[deploy] validating python, js syntax, & hub e2e..."
python3 -m py_compile "$ROOT"/ui/hub/*.py
python3 "$ROOT/ui/tests/test_hub_e2e.py"
python3 "$ROOT/ui/tests/test_hub_live_e2e.py"
node -e '
const fs = require("fs");
for (const f of ["ui/static/shell.js", "ui/static/settings.js", "ui/static/space.js"]) {
  new Function(fs.readFileSync(f, "utf8"));
}
for (const h of ["ui/static/home.html", "ui/static/index.html"]) {
  const html = fs.readFileSync(h, "utf8");
  for (const m of html.matchAll(/<script(?:\s[^>]*)?>([\s\S]*?)<\/script>/g)) {
    if (m[1].trim()) new Function(m[1]);
  }
}
'

if [[ "$WITH_BINARIES" -eq 1 ]]; then
  echo "[deploy] building release binaries..."
  (cd "$ROOT" && cargo build --release -p playbox-cli -p playbox-mcp)
  for dir in "$HOME/.cargo/bin" "$HOME/.local/bin"; do
    [[ -d "$dir" ]] || continue
    cp "$ROOT/target/release/playbox" "$dir/playbox.new" && mv "$dir/playbox.new" "$dir/playbox"
    cp "$ROOT/target/release/playbox-mcp" "$dir/playbox-mcp.new" && mv "$dir/playbox-mcp.new" "$dir/playbox-mcp"
  done
fi

echo "[deploy] syncing ui/ -> $HUB_CUR ..."
mkdir -p "$HUB_CUR/hub" "$HUB_CUR/static"
cp "$ROOT"/ui/hub/*.py "$HUB_CUR/hub/"
cp "$ROOT"/ui/static/*.html "$ROOT"/ui/static/*.js "$ROOT"/ui/static/*.css "$HUB_CUR/static/"

echo "[deploy] restarting com.playbox.hub..."
launchctl kickstart -k "gui/$(id -u)/com.playbox.hub"
sleep 1

echo "[deploy] smoke check..."
launchctl list | grep com.playbox.hub
curl -fsS http://127.0.0.1:7717/manifest.webmanifest >/dev/null && echo "[deploy] hub HTTP ok"
