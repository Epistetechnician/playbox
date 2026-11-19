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
RELEASE_PUBKEY="98de00e615455df4e38693740a1471bf8a9385741afa37bad4d6af1ae985a7e4"

say() { printf '%s\n' "$*"; }
die() { printf 'playbox install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
shq() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

hex_to_raw() {
  h="$1"
  oct=""
  while [ -n "$h" ]; do
    b="${h%"${h#??}"}"
    h="${h#??}"
    oct="$oct\\$(printf '%03o' "$((0x$b))")"
  done
  printf "$oct"
}

verify_sig() {
  pub_hex="$1" msg_file="$2" sig_file="$3"
  if have python3; then
    python3 - "$pub_hex" "$msg_file" "$sig_file" <<'PY'
import hashlib, pathlib, sys
P, L = 2**255 - 19, 2**252 + 27742317777372353535851937790883648493
inv = lambda x: pow(x, P - 2, P)
D, I = -121665 * inv(121666) % P, pow(2, (P - 1) // 4, P)
def xrec(y):
    xx = (y * y - 1) * inv(D * y * y + 1) % P
    x = pow(xx, (P + 3) // 8, P)
    if (x * x - xx) % P: x = x * I % P
    if (x * x - xx) % P: raise ValueError
    return P - x if x & 1 else x
def dec(b):
    n = int.from_bytes(b, "little")
    s, y = n >> 255, n & ((1 << 255) - 1)
    if y >= P: raise ValueError
    x = xrec(y)
    if x == 0 and s == 1: raise ValueError
    if x & 1 != s: x = P - x
    return (x, y, 1, x * y % P)
def add(p, q):
    a, b = (p[1] - p[0]) * (q[1] - q[0]) % P, (p[1] + p[0]) * (q[1] + q[0]) % P
    c, d = 2 * p[3] * q[3] * D % P, 2 * p[2] * q[2] % P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % P, g * h % P, f * g % P, e * h % P)
def mul(s, p):
    q = (0, 1, 1, 0)
    while s:
        if s & 1: q = add(q, p)
        p, s = add(p, p), s >> 1
    return q
BY = 4 * inv(5) % P
B = (xrec(BY), BY, 1, xrec(BY) * BY % P)
ph, sh = sys.argv[1].strip(), pathlib.Path(sys.argv[3]).read_text().strip()
assert len(ph) == 64 and len(sh) == 128 and all(c in "0123456789abcdefABCDEF" for c in ph + sh)
pub, sig, msg = bytes.fromhex(ph), bytes.fromhex(sh), pathlib.Path(sys.argv[2]).read_bytes()
a, r = dec(pub), dec(sig[:32])
s = int.from_bytes(sig[32:], "little")
a8 = mul(8, a)
assert s < L and not (a8[0] % P == 0 and a8[1] % P == a8[2] % P)
h = int.from_bytes(hashlib.sha512(sig[:32] + pub + msg).digest(), "little") % L
u, v = mul(8 * s, B), add(mul(8, r), mul(h, a8))
assert u[0] * v[2] % P == v[0] * u[2] % P and u[1] * v[2] % P == v[1] * u[2] % P
PY
  elif have openssl; then
    sig_hex="$(tr -d ' \t\r\n' < "$sig_file")"
    case "$pub_hex" in *[!0-9a-fA-F]*|"") return 1 ;; esac
    case "$sig_hex" in *[!0-9a-fA-F]*|"") return 1 ;; esac
    [ "${#pub_hex}" -eq 64 ] && [ "${#sig_hex}" -eq 128 ] || return 1
    {
      printf '%s\n' "-----BEGIN PUBLIC KEY-----"
      hex_to_raw "302a300506032b6570032100$pub_hex" | openssl base64 -A
      printf '\n%s\n' "-----END PUBLIC KEY-----"
    } > "$tmp/release.pub.pem"
    hex_to_raw "$sig_hex" > "$tmp/SHA256SUMS.sig.bin"
    openssl pkeyutl -verify -pubin -inkey "$tmp/release.pub.pem" -rawin -in "$msg_file" -sigfile "$tmp/SHA256SUMS.sig.bin" >/dev/null 2>&1
  else
    die "need python3 or openssl to verify the release signature"
  fi
}

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
if [ -n "$join" ]; then case "$join" in playbox://*|https://*/i/*|http://*/i/*) ;; *) die "that doesn't look like a Playbox invite (a playbox:// link or an invite page address)" ;; esac; fi

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

# 1. The checksum list is signed by the maintainer's key (pinned above), verified on the host BEFORE extracting or executing
#    anything from the downloaded archive. A pinned key makes the signature mandatory: a release without one is refused, so
#    stripping the .sig from a mirror cannot downgrade the check. `playbox update` pins the same key (release.pub) inside the
#    binary. PLAYBOX_ALLOW_UNSIGNED=1 installs an old unsigned release (0.2.1 and earlier) on purpose.
if [ -n "$RELEASE_PUBKEY" ]; then
  if curl -fsSL "$base/SHA256SUMS.sig" -o "$tmp/SHA256SUMS.sig" 2>/dev/null; then
    verify_sig "$RELEASE_PUBKEY" "$tmp/SHA256SUMS" "$tmp/SHA256SUMS.sig" || die "signature check failed; not installing"
    say "signature ok"
  elif [ "${PLAYBOX_ALLOW_UNSIGNED:-}" = 1 ]; then
    say "warning: this release is unsigned; installing because PLAYBOX_ALLOW_UNSIGNED=1"
  else
    die "this release is not signed. Releases from 0.3.0 on are; for an older one set PLAYBOX_ALLOW_UNSIGNED=1"
  fi
fi

# 2. The download matches the verified checksum list.
want="$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")"
[ -n "$want" ] || die "$asset is not in SHA256SUMS"
if have sha256sum; then got="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
elif have shasum; then got="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
else die "need sha256sum or shasum to check the download"; fi
[ "$got" = "$want" ] || die "checksum mismatch for $asset (expected $want, got $got); not installing"

tar -xzf "$tmp/$asset" -C "$tmp"
[ -x "$tmp/playbox" ] || die "the archive has no playbox binary"

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
  printf '#!/bin/sh\nPATH=%s:"$PATH" exec python3 %s "$@"\n' "$(shq "$dest")" "$(shq "$hubdir/ui/playbox_hub.py")" > "$dest/playbox-hub"
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
