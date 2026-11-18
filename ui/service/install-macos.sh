#!/bin/sh
# Install the Playbox hub as a macOS LaunchAgent: starts at login, restarts if it dies, takes its Cloudflare tunnel down with it.
#   ui/service/install-macos.sh --cloudflare-tunnel ID --cloudflare-hostname HOST [--cloudflare-access TEAM --cloudflare-aud AUD]
#   ui/service/install-macos.sh --uninstall
#   ui/service/install-macos.sh --repin     # run the hub code of THIS checkout from now on, then restart the hub (plist and flags unchanged)
# The service runs a pinned copy of the hub in ~/.playbox/hub-app/current, not this working tree: switching branches or editing files here
# does not change what is running. Add --live to run straight from this checkout instead (development).
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
APP="$HOME/.playbox/hub-app"
pin() {     # copy this checkout's hub into a new snapshot directory and point `current` at it; keep the last three
  stamp="$(date +%Y%m%d-%H%M%S)-$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo nogit)"
  mkdir -p "$APP/$stamp"
  echo "$stamp" > "$APP/$stamp/BUILD"
  (cd "$HERE" && tar -cf - --exclude=__pycache__ --exclude='*.pyc' --exclude=tests --exclude=e2e.py --exclude=service .) | (cd "$APP/$stamp" && tar -xf -)
  ln -sfn "$APP/$stamp" "$APP/current.new" && mv -fh "$APP/current.new" "$APP/current" 2>/dev/null || { rm -f "$APP/current"; ln -s "$APP/$stamp" "$APP/current"; }
  ls -1dt "$APP"/2* 2>/dev/null | tail -n +4 | while read -r old; do rm -rf "$old"; done
  echo "pinned the hub at $APP/$stamp"
}
if [ "${1:-}" = "--repin" ]; then
  pin
  launchctl kickstart -k "gui/$(id -u)/$LABEL" && echo "restarted $LABEL"; exit 0
fi
xml_esc() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }
ENTRY="$APP/current/playbox_hub.py"
if [ "${1:-}" = "--live" ]; then shift; ENTRY="$HERE/playbox_hub.py"; else pin; fi
ARGS=""
for a in "$@"; do ARGS="$ARGS
    <string>$(xml_esc "$a")</string>"; done
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
(umask 077 && : >> "$LOG") && chmod 600 "$LOG"
cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$(xml_esc "$LABEL")</string>
  <key>ProgramArguments</key><array>
    <string>$(xml_esc "$PY")</string><string>$(xml_esc "$ENTRY")</string>$ARGS
  </array>
  <key>EnvironmentVariables</key><dict>
    <key>PATH</key><string>$(xml_esc "$HOME")/.cargo/bin:$(xml_esc "$HOME")/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>$(xml_esc "$LOG")</string>
  <key>StandardErrorPath</key><string>$(xml_esc "$LOG")</string>
</dict></plist>
PL
chmod 600 "$PLIST"
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
for try in 1 2 3 4 5; do launchctl bootstrap "gui/$(id -u)" "$PLIST" 2>/dev/null && break; sleep 2; done      # the old job needs a moment to unload
launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || { echo "could not start $LABEL" >&2; exit 1; }
echo "installed $LABEL (log: $LOG)"
