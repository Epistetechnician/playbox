# Fuzzing

Coverage-guided fuzzing (libFuzzer) of everything a stranger, a member, or a host being judged can hand us as
bytes or text. Needs `cargo install cargo-fuzz` and a nightly toolchain.

```sh
cd fuzz
cargo +nightly run --bin seeds        # write valid starting inputs into corpus/ (once)
./run-all.sh                          # every target, 60 s each (pass seconds as the first argument)
cargo +nightly fuzz run wire_frame    # one target, until you stop it
```

| Target | What it feeds | Who controls the bytes |
|---|---|---|
| `wire_frame` | request frame → A2A message → Playbox operation | any stranger, any member |
| `grant_token` | `authorize` / `verify_invite` (Biscuit parsing) | any member, any stranger |
| `revocation_notice` | `Notice::parse` | any peer on gossip or pull |
| `attestation_statement` | `Statement::parse`, `attest::verify` | the host a guest is judging |
| `history_and_audit` | history logs, the audit-log verifier | files a peer sent, a tampered log |
| `links_and_pem` | join links, peer addresses, transports, PEM/DER bundles, approver lists | whoever sent you a link |
| `tpm_evidence` | TPM key blobs, EK certificate chains (X.509), the TPM and Secure Enclave verifiers | the host being judged |
| `tdx_token` | the Confidential Space token verifier (JWT) | the host being judged |

Beyond libFuzzer, two deterministic mutation tests run in `cargo test`:

- `crates/playbox-net/tests/fuzz_host.rs` sends 15 000 mangled requests, from a real member's identity so some get
  past authorization into the pull, push and task handlers, to a running host with no task runner, then checks the host
  still answers, every reply is well-formed JSON-RPC, and its audit log still verifies.
- `crates/playbox-git` has a test that damages a real git bundle 120 ways and checks nothing hangs and the repository
  is still `fsck`-clean.

A crash found by libFuzzer lands in `fuzz/artifacts/<target>/`; replay with `cargo +nightly fuzz run <target> <file>`.
Turn a real find into a regular test before fixing it.

## Not covered

- `git` itself parses bundles and patches; we test around it (damaged bundles, hostile patches), we do not fuzz git.
- The TPM and Secure Enclave verifiers are fuzzed at their parsing and verification entry points only, not against
  real hardware.
- No long runs: each target ran for minutes, not days.
