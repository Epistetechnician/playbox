#!/bin/sh
# Build the Secure Enclave helper. Usage: tools/playbox-se/build.sh [OUTPUT]   (default: ~/.cargo/bin/playbox-se)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-"$HOME/.cargo/bin/playbox-se"}
[ "$(uname)" = Darwin ] || { echo "playbox-se is macOS only" >&2; exit 1; }
swiftc -O "$here/playbox-se.swift" -o "$out"
echo "built $out"
