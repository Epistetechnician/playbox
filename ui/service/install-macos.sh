#!/bin/sh
# Install the Playbox hub as a macOS LaunchAgent: starts at login, restarts if it dies, takes its Cloudflare tunnel down with it.
#   ui/service/install-macos.sh --cloudflare-tunnel ID --cloudflare-hostname HOST [--cloudflare-access TEAM --cloudflare-aud AUD]
#   ui/service/install-macos.sh --uninstall
# Never put --pair here: a pairing link would end up in the log. To pair a device, stop the service and run the hub by hand with --pair.
set -eu
LABEL=com.playbox.hub
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/playbox-hub.log"
if [ "${1:-}" = "--uninstall" ]; then
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"; echo "removed $LABEL"; exit 0
fi
HERE=$(cd "$(dirname "$0")/.." && pwd)
PY=$(command -v python3)
ARGS=""
for a in "$@"; do ARGS="$ARGS
    <string>$a</string>"; done
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array>
    <string>$PY</string><string>$HERE/playbox_hub.py</string>$ARGS
  </array>
  <key>EnvironmentVariables</key><dict>
    <key>PATH</key><string>$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict></plist>
PL
chmod 600 "$PLIST"
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "installed $LABEL (log: $LOG)"
