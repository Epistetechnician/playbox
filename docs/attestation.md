# Attestation

What a room can honestly say about the machines in it, and what it cannot yet.

## Statements

A **statement** (`playbox-core::attest`) is signed text by a key, about that key: its software
measurements (blake3 of the running binary, the runner, a hash of the room's policy, OS, arch),
an optional nonce, an optional history head, and optional platform evidence. Because the key signs
its own statement, a bare statement proves only that the key holder *claims* this. Tiers say what a
verifier may *believe*:

| Tier | Means | Achievable today |
|---|---|---|
| T0 | The holder of the key signed these measurements | yes |
| T1 | A device-bound key / platform quote shows the same physical device | no: no verifier |
| T2 | T1, and the quote covers sandbox image, kernel and policy | no: no verifier |
| T3 | A confidential-VM report covers the whole host | no: no verifier |

`attest::verify` returns the **achieved** tier: T0 unless a `PlatformVerifier` for the statement's
`platform` kind checks the evidence, and never above what was claimed. Evidence with no verifier is
kept and reported but not believed. Writing the TPM, Secure Enclave, SEV-SNP and TDX verifiers is
open work and needs hardware to test on.

## Host attestation

Any peer can call `playbox.GetAttestation` with a fresh hex nonce (no invite needed; it is public like
the agent card). The host signs a statement with its endpoint key. `Client::verify_host` accepts it
only if the subject is the key the iroh connection already authenticated, the nonce is ours, and the
achieved tier meets the caller's minimum. The MCP `playbox_join` does this before showing the invite
(`min_host_tier`, default `t0`), so a host that fails never sees it, and records the statement in the
guest's history as `host_attestation`.

What this shows: the host is running a binary with this hash and this policy hash, as claimed by the
host key. It does **not** show the binary is the one you expect (compare the hash yourself) or that the
host is not lying: a malicious host signs whatever it likes. That is what T1-T3 evidence is for.

## Histories

`playbox attest` appends a self-attestation to a history log, with the payload kept beside it
(`<log>.payloads/`). `playbox verify <log>` (and the MCP `playbox_verify`) checks:

- the hash chain and every signature;
- each `attestation` entry: valid signature, subject equals the log's key, and it was issued at the
  position where it sits (its `log_head` is the hash of the entry before it), so it cannot be replayed
  later in the log;
- each `host_attestation` entry: valid signature;
- that kept bodies match their hashes. A missing or altered body is a problem and `verify` exits non-zero.

Not checked yet: that `commit` entries exist in a repository, and that the grant chain was valid at each
tool call (ordinary entries commit to a payload hash but the bodies are not kept).

## Policy

`attest::Policy` maps an operation to a minimum tier (`Policy::default().require("merge", Tier::T2)`).
It is not enforced by the host yet; with only T0 available, any requirement above T0 can't be met.
