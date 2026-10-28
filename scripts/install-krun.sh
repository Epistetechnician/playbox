#!/bin/sh
# Build and install the libkrun helper, signed for Hypervisor.framework on macOS.
#   macOS prerequisite:  brew tap libkrun/krun && brew trust libkrun/krun && brew install libkrun
#   Linux prerequisite:  libkrun and libkrunfw installed (needs /dev/kvm access)
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here"
cargo install --path crates/playbox-runner --features krun --bin playbox-krun-helper --locked --force
bin="${CARGO_HOME:-$HOME/.cargo}/bin/playbox-krun-helper"
if [ "$(uname)" = Darwin ]; then
    codesign --force --sign - --entitlements scripts/krun.entitlements "$bin"
    echo "signed $bin with the hypervisor entitlement (ad hoc)"
fi
echo "installed $bin"
