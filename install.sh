#!/bin/sh
# Install Playbox: download a release for this machine, check it, put `playbox` in ~/.local/bin, and register
# it with the coding agents you have (Claude Code, Codex, OpenCode, Hermes).
#
#   curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh
#
# Environment: PLAYBOX_VERSION (default: latest), PLAYBOX_INSTALL (default ~/.local/bin),
#              PLAYBOX_NO_SETUP=1 (skip registering agents), PLAYBOX_BASE_URL (mirror or test server).
# Needs no sudo. Read it first if you like: it is short.
set -eu

REPO="Epistetechnician/playbox"
# Release signing key (ed25519, hex). Empty until the maintainer pins one with `playbox release keygen`.
RELEASE_PUBKEY=""

say() { printf '%s\n' "$*"; }
die() { printf 'playbox install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

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
