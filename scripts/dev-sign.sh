#!/bin/sh
# Sign playbox binaries with ONE stable local identity, so macOS keeps its answer to "accept incoming network connections?" across rebuilds.
#
# An ad-hoc signature (what `cargo build` and `codesign -s -` give you) is the hash of that exact build, so every build is a new program to the
# firewall and to the keychain. A signature made with a fixed certificate has a stable designated requirement
# (`identifier "..." and certificate leaf = H"..."`), which is what the firewall remembers.
#
#   sh scripts/dev-sign.sh                      # sign target/debug and target/release binaries, and the installed ones
#   sh scripts/dev-sign.sh path/to/playbox ...  # sign these
#   sh scripts/dev-sign.sh --setup              # only create the identity (once; no sudo, no password)
#   sh scripts/dev-sign.sh --status             # show the identity and each binary's requirement
#
# Then allow each program once with `sh scripts/macos-allow.sh <path>` (sudo). After that, rebuilding and re-signing with this identity does not
# ask again. To skip the firewall altogether, run with PLAYBOX_BIND=loopback.
#
# The identity is local to this Mac and trusted by nothing: it never leaves your login keychain and is only used to name your own builds.
set -eu
[ "$(uname -s)" = Darwin ] || { echo "macOS only" >&2; exit 1; }
NAME="${PLAYBOX_SIGN_IDENTITY:-Playbox Dev Signing}"
KC="$(security default-keychain | tr -d ' "')"

have_identity() { security find-certificate -c "$NAME" "$KC" >/dev/null 2>&1; }

record() {            # `playbox update` signs the new binary with this identity when this file names one
    mkdir -p "$HOME/.playbox"
    printf '%s\n' "$NAME" > "$HOME/.playbox/sign-identity"
}

setup() {
    if have_identity; then
        echo "identity \"$NAME\" is already in $KC"
        record
        return 0
    fi
    d="$(mktemp -d)"
    trap 'rm -rf "$d"' EXIT
    cat > "$d/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF
    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$d/key.pem" -out "$d/cert.pem" -config "$d/cert.cnf" 2>/dev/null
    pw="$(openssl rand -hex 12)"
    # Legacy PKCS#12 encryption: macOS `security import` cannot read the OpenSSL 3 default.
    openssl pkcs12 -export -legacy -inkey "$d/key.pem" -in "$d/cert.pem" -name "$NAME" -out "$d/id.p12" -passout "pass:$pw" 2>/dev/null \
        || openssl pkcs12 -export -inkey "$d/key.pem" -in "$d/cert.pem" -name "$NAME" -out "$d/id.p12" -passout "pass:$pw"
    # -T lets codesign use the key without a prompt each time.
    security import "$d/id.p12" -k "$KC" -P "$pw" -T /usr/bin/codesign >/dev/null
    echo "created identity \"$NAME\" in $KC"
    record
}

sign_one() {
    bin="$1"
    out="$(mktemp)"
    [ -f "$bin" ] || return 0
    id="com.playbox.$(basename "$bin" | sed 's/[^A-Za-z0-9._-]/-/g')"
    # codesign waits forever if the login keychain is locked or macOS is asking whether it may use the key; give up after 20 s and say what to do.
    codesign --force --sign "$NAME" --identifier "$id" --timestamp=none "$bin" > "$out" 2>&1 &
    pid=$!
    n=0
    while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 40 ]; do sleep 0.5; n=$((n + 1)); done
    if kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        echo "NOT signed: $bin" >&2
        echo "  codesign is waiting on the keychain. Unlock your login keychain (Keychain Access) or click Always Allow on the dialog about \"$NAME\", then run this again." >&2
        return 1
    fi
    wait "$pid" || { echo "NOT signed: $bin ($(grep -v 'replacing existing signature' "$out" | head -1))" >&2; return 1; }
    echo "signed $bin"
}

status() {
    have_identity && echo "identity: \"$NAME\" (present)" || echo "identity: \"$NAME\" (missing: run --setup)"
    shift || true
    for b in "$@"; do
        [ -f "$b" ] && { printf '%s\n   ' "$b"; codesign -dr - "$b" 2>&1 | sed -n 's/^# //p;s/^designated => //p' | head -1; }
    done
}

root="$(cd "$(dirname "$0")/.." && pwd)"
default_bins() {
    for f in "$root"/target/debug/playbox "$root"/target/debug/playbox-mcp "$root"/target/release/playbox "$root"/target/release/playbox-mcp \
             "$HOME/.local/bin/playbox" "$HOME/.cargo/bin/playbox" "$HOME/.cargo/bin/playbox-mcp"; do
        [ -f "$f" ] && echo "$f"
    done
}

case "${1:-}" in
    --setup) setup ;;
    --status) shift; if [ "$#" -eq 0 ]; then set -- $(default_bins); fi; status x "$@" ;;
    *)
        setup
        if [ "$#" -eq 0 ]; then set -- $(default_bins); fi
        rc=0
        for b in "$@"; do sign_one "$b" || rc=1; done
        exit "$rc"
        ;;
esac
