#!/bin/sh
# Dry run of the GPU attestation path with NO GPU: a fake NVIDIA attestation service (crates/playbox-core/examples/fake_nras.rs)
# signs tokens with a made-up key, the host produces `nvidia-gpu` evidence from it, and `playbox verify` must accept the good
# run and reject every deliberately broken one. Proves the plumbing, not the trust: nothing here says a real H100 exists.
#   scripts/gpu-attest-dryrun.sh
set -eu
cd "$(dirname "$0")/.."
cargo build -q -p playbox-cli -p playbox-core --example fake_nras 2>/dev/null || { cargo build -q -p playbox-cli; cargo build -q -p playbox-core --example fake_nras; }
PB=$PWD/target/debug/playbox; FAKE=$PWD/target/debug/examples/fake_nras
PLAYBOX_HOME=$(mktemp -d); export PLAYBOX_HOME; trap 'rm -rf "$PLAYBOX_HOME"' EXIT
"$FAKE" jwks > "$PLAYBOX_HOME/gpu-jwks.json"
LOG=$PLAYBOX_HOME/history/default.log
fail=0
check() { # name expected-tier [VAR=value ...]
  name=$1; want=$2; shift 2; rm -f "$LOG"
  env "$@" PLAYBOX_ATTEST=gpu PLAYBOX_GPU_EVIDENCE_CMD="$FAKE" "$PB" attest >/dev/null
  got=$(PLAYBOX_GPU_JWKS="$PLAYBOX_HOME/gpu-jwks.json" PLAYBOX_GPU_DRIVERS=550.90.07 PLAYBOX_GPU_VBIOS=96.00.74.00.1A "$PB" verify "$LOG" | sed -n 's/.*verified \(T[0-3]\).*/\1/p' | head -1)
  if [ "$got" = "$want" ]; then echo "ok    $name -> $got"; else echo "FAIL  $name: wanted $want, got ${got:-nothing}"; fail=1; fi
}
check "genuine, pinned firmware" T3 A=1
for b in debug secboot measres model nonce driver; do check "broken: $b" T0 FAKE_NRAS_BREAK=$b; done
check "other firmware version" T0 FAKE_NRAS_DRIVER=999.1
check "two GPUs" T3 FAKE_NRAS_GPUS=2
exit $fail
