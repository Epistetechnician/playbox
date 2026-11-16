#!/bin/sh
# Keep a Playbox room host running as a macOS LaunchAgent: starts at login, restarts if it dies, survives closing the terminal.
#   ui/service/install-host-macos.sh NAME [playbox up options...]     e.g.  gpu-lab --state ~/.playbox-rooms/gpu-lab --repo ~/work/gpu-lab
#   ui/service/install-host-macos.sh --uninstall NAME
#   ui/service/install-host-macos.sh --list
# The room keeps its identity in --state (default ~/.playbox-rooms/NAME), so members and bans survive restarts. The host publishes itself in
# ~/.playbox/hosting, so the hub can adopt it and mint invite links over its control socket: no console, no link in any log.
set -eu
if [ "${1:-}" = "--list" ]; then
  launchctl list | grep -E 'com\.playbox\.host\.' || echo "no hosts installed"; exit 0
fi
if [ "${1:-}" = "--uninstall" ]; then
  [ -n "${2:-}" ] || { echo "usage: $0 --uninstall NAME" >&2; exit 2; }
  L="com.playbox.host.$2"
  launchctl bootout "gui/$(id -u)/$L" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/$L.plist"; echo "removed $L"; exit 0
fi
NAME="${1:-}"
case "$NAME" in ""|-*|*[!A-Za-z0-9._-]*) echo "usage: $0 NAME [playbox up options...]   (NAME: letters, digits, . _ -)" >&2; exit 2;; esac
shift
BIN=$(command -v playbox) || { echo "playbox is not on PATH" >&2; exit 1; }
LABEL="com.playbox.host.$NAME"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/playbox-host-$NAME.log"
STATE="$HOME/.playbox-rooms/$NAME"
for a in "$@"; do [ "$a" = "--state" ] && STATE=""; done      # the caller chose one
ARGS="<string>up</string>"
[ -n "$STATE" ] && ARGS="$ARGS<string>--state</string><string>$STATE</string>"
for a in "$@"; do ARGS="$ARGS<string>$a</string>"; done
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
[ -n "$STATE" ] && mkdir -p "$STATE" && chmod 700 "$STATE"
cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$BIN</string>$ARGS</array>
  <key>EnvironmentVariables</key><dict>
    <key>PATH</key><string>$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
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
for try in 1 2 3 4 5; do launchctl bootstrap "gui/$(id -u)" "$PLIST" 2>/dev/null && break; sleep 2; done      # the old job needs a moment to unload
launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || { echo "could not start $LABEL" >&2; exit 1; }
echo "installed $LABEL (log: $LOG)"
echo "the log holds the first join link: treat it as a secret, or mint links from the hub instead"
