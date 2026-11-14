#!/bin/sh
# macOS asks "Do you want the application "playbox" to accept incoming network connections?" once per program,
# and again after every update because the new binary is a new program. This tells the firewall once, for the
# installed binary, so it stops asking (needs sudo; run it again after `playbox update`).
#
#   sh scripts/macos-allow.sh [path-to-playbox]
#
# To avoid the prompt without touching the firewall, keep the host off the network instead:
#   PLAYBOX_BIND=loopback playbox mcp      # listens on 127.0.0.1 only; members still connect through the relay
set -eu
bin="${1:-$(command -v playbox || true)}"
[ -n "$bin" ] && [ -x "$bin" ] || { echo "playbox not found; pass its path" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || { echo "macOS only" >&2; exit 1; }
fw=/usr/libexec/ApplicationFirewall/socketfilterfw
sudo "$fw" --add "$bin"
sudo "$fw" --unblockapp "$bin"
echo "allowed: $bin"
