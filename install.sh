#!/bin/sh
# Install Playbox: download a release for this machine, check it, put `playbox` in ~/.local/bin, and register
# it with the coding agents you have (Claude Code, Codex, OpenCode, Hermes).
#
#   curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh
#
# Environment: PLAYBOX_VERSION (default: latest), PLAYBOX_INSTALL (default ~/.local/bin),
#              PLAYBOX_NO_SETUP=1 (skip registering agents), PLAYBOX_BASE_URL (mirror or test server).
# Needs no sudo. Read it first if you like: it is short.
#
# Joining a friend's room from an invite page: also installs the Playbox hub (a local dashboard with passkey sign-in),
# joins the room and opens the pairing page in your browser. Leave that terminal open while you use it.
#
#   curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh -s -- --join 'playbox://...' --name 'Room name'
#
# Options: --join LINK, --name NAME, --hub (install and start the hub without an invite), --no-start (install, don't launch).
# Environment: PLAYBOX_HUB_DIR (default ~/.local/share/playbox-hub).
set -eu

REPO="Epistetechnician/playbox"
# Release signing key (ed25519, hex). Empty until the maintainer pins one with `playbox release keygen`.
RELEASE_PUBKEY=""

say() { printf '%s\n' "$*"; }
die() { printf 'playbox install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

join="${PLAYBOX_JOIN_LINK:-}" name="${PLAYBOX_JOIN_NAME:-}" want_hub=0 start=1
while [ $# -gt 0 ]; do
  case "$1" in
    --join) [ $# -ge 2 ] || die "--join needs the playbox:// link"; join="$2"; want_hub=1; shift 2 ;;
    --name) [ $# -ge 2 ] || die "--name needs a value"; name="$2"; shift 2 ;;
    --hub) want_hub=1; shift ;;
    --no-start) start=0; shift ;;
    *) die "unknown option $1" ;;
  esac
done
if [ -n "$join" ]; then case "$join" in playbox://*) ;; *) die "that doesn't look like a playbox:// invite link" ;; esac; fi

have curl || die "curl is required"
have tar || die "tar is required"

case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  *) die "unsupported OS $(uname -s); build from source: cargo install --path crates/playbox-cli" ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch=arm64 ;;
  x86_64|amd64) arch=x86_64 ;;
  *) die "unsupported CPU $(uname -m)" ;;
esac
asset="playbox-$os-$arch.tar.gz"

if [ -n "${PLAYBOX_BASE_URL:-}" ]; then
  base="$PLAYBOX_BASE_URL"
elif [ -n "${PLAYBOX_VERSION:-}" ]; then
  base="https://github.com/$REPO/releases/download/$PLAYBOX_VERSION"
else
  base="https://github.com/$REPO/releases/latest/download"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset"
curl -fsSL "$base/$asset" -o "$tmp/$asset" || die "could not download $base/$asset (is there a release for $os-$arch?)"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || die "could not download SHA256SUMS"

# 1. The download matches the checksum list.
want="$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")"
[ -n "$want" ] || die "$asset is not in SHA256SUMS"
if have sha256sum; then got="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
elif have shasum; then got="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
else die "need sha256sum or shasum to check the download"; fi
[ "$got" = "$want" ] || die "checksum mismatch for $asset (expected $want, got $got); not installing"

tar -xzf "$tmp/$asset" -C "$tmp"
[ -x "$tmp/playbox" ] || die "the archive has no playbox binary"

# 2. The checksum list is signed by the maintainer's key. This uses the downloaded binary, so on its own it
#    only guards against a mismatched or corrupted upload; `playbox` verifies later updates with the key it
#    already trusts. It is skipped when no key is pinned or the release has no signature.
if [ -n "$RELEASE_PUBKEY" ] && curl -fsSL "$base/SHA256SUMS.sig" -o "$tmp/SHA256SUMS.sig" 2>/dev/null; then
  "$tmp/playbox" release verify "$tmp/SHA256SUMS" --pubkey "$RELEASE_PUBKEY" >/dev/null || die "signature check failed; not installing"
  say "signature ok"
fi

dest="${PLAYBOX_INSTALL:-$HOME/.local/bin}"
mkdir -p "$dest"
install -m 755 "$tmp/playbox" "$dest/playbox"
say "installed $dest/playbox ($("$dest/playbox" --version))"

case ":$PATH:" in
  *":$dest:"*) ;;
  *) say "note: $dest is not on your PATH. Add it:  export PATH=\"$dest:\$PATH\"" ;;
esac

if [ "${PLAYBOX_NO_SETUP:-}" = 1 ]; then
  say "skipped agent setup; run: $dest/playbox setup"
else
  "$dest/playbox" setup
fi

# 3. The hub: the dashboard, passkeys and the invite pages. Same checksum list as the binary.
if [ "$want_hub" = 1 ]; then
  have python3 || die "the hub needs python3 (macOS: xcode-select --install; Debian/Ubuntu: apt install python3)"
  hubdir="${PLAYBOX_HUB_DIR:-$HOME/.local/share/playbox-hub}"
  if ! curl -fsSL "$base/playbox-hub.tar.gz" -o "$tmp/playbox-hub.tar.gz" 2>/dev/null; then
    die "this release has no hub bundle yet. From a checkout instead:  git clone https://github.com/$REPO && python3 playbox/ui/playbox_hub.py --open"
  fi
  hwant="$(awk '$2 == "playbox-hub.tar.gz" || $2 == "*playbox-hub.tar.gz" { print $1 }' "$tmp/SHA256SUMS")"
  [ -n "$hwant" ] || die "playbox-hub.tar.gz is not in SHA256SUMS; not installing the hub"
  if have sha256sum; then hgot="$(sha256sum "$tmp/playbox-hub.tar.gz" | awk '{print $1}')"; else hgot="$(shasum -a 256 "$tmp/playbox-hub.tar.gz" | awk '{print $1}')"; fi
  [ "$hgot" = "$hwant" ] || die "checksum mismatch for playbox-hub.tar.gz; not installing the hub"
  rm -rf "$hubdir.new"; mkdir -p "$hubdir.new"
  tar -xzf "$tmp/playbox-hub.tar.gz" -C "$hubdir.new"
  [ -f "$hubdir.new/ui/playbox_hub.py" ] || die "the hub bundle has no ui/playbox_hub.py"
  rm -rf "$hubdir"; mv "$hubdir.new" "$hubdir"
  printf '#!/bin/sh\nPATH="%s:$PATH" exec python3 "%s/ui/playbox_hub.py" "$@"\n' "$dest" "$hubdir" > "$dest/playbox-hub"
  chmod 755 "$dest/playbox-hub"
  say "installed the hub: $dest/playbox-hub"
  if [ "$start" = 1 ]; then
    say ""
    say "starting the hub. Leave this window open; open the page it shows to set up your passkey."
    rm -rf "$tmp"
    if [ -n "$join" ]; then
      PLAYBOX_JOIN_LINK="$join" PLAYBOX_JOIN_NAME="$name" exec "$dest/playbox-hub" --open
    fi
    exec "$dest/playbox-hub" --open
  fi
fi
