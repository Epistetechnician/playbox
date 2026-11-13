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
| `latent_envelope` | the `PBL2` hub-vector envelope, the reveal that opens its commitment, the host's receipt | any member, or a host a member is judging |
| `capability_bind` | an owner's capability definition and a member's arguments: the program never changes, no control characters | the owner's file, any member |
| `room_state_files` | the calls and conversations files a host reads at start | a damaged or edited state directory |
| `latent_store` | the saved latent mailbox a host reads back at start | a damaged or edited state directory |

Beyond libFuzzer, deterministic mutation tests run in the normal test suites:

- `crates/playbox-net/tests/fuzz_host.rs` sends 15 000 mangled requests, from a real member's identity so some get
  past authorization into the pull, push and task handlers, to a running host with no task runner, then checks the host
  still answers, every reply is well-formed JSON-RPC, and its audit log still verifies.
  It also sends the room's talk and latent-mailbox operations (`say`, `feed`, signed `latent` posts, reveals, pulls), then
  stops the host and starts it again to check the mailbox it saved is still a valid store.
- `steer/tests/test_hub_fuzz.py` mutates the hub-message parsers (`unpack`, `unpack_signed`, the audit's reveal handling) and
  `steer/tests/test_handoff_fuzz.py` mutates the KV-cache handoff receiver: whatever the bytes, the answer is a refusal
  (`ValueError`, or a `Decision`), never another exception, and nothing mutated is accepted.
- `crates/playbox-git` has a test that damages a real git bundle 120 ways and checks nothing hangs and the repository
  is still `fsck`-clean.

A crash found by libFuzzer lands in `fuzz/artifacts/<target>/`; replay with `cargo +nightly fuzz run <target> <file>`.
Turn a real find into a regular test before fixing it.

## Not covered

- `git` itself parses bundles and patches; we test around it (damaged bundles, hostile patches), we do not fuzz git.
- The TPM and Secure Enclave verifiers are fuzzed at their parsing and verification entry points only, not against
  real hardware.
- No long runs: each target ran for minutes, not days.
