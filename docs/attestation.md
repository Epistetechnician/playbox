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
| T1 | A pinned device key (Secure Enclave or TPM) signed or quoted this statement | yes, once the key is pinned |
| T2 | T1, and a TPM quote shows PCR 23 holds the host's measurements and boot PCRs match your reference | yes, with a TPM and reference values |
| T3 | A confidential-VM report covers the whole host | no: no verifier |

`attest::verify` returns the **achieved** tier: T0 unless a `PlatformVerifier` for the statement's
`platform` kind checks the evidence, and never above what was claimed. Evidence with no verifier is
kept and reported but not believed. Verifiers exist for the Secure Enclave and TPM 2.0 (below); SEV-SNP and
TDX verifiers are open work and need a confidential VM to test on.

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

## Platform evidence (hardware)

Statements can carry evidence from a device-bound key. Two kinds exist, and verifiers for both are pure
Rust, so a Mac can verify a Linux host's TPM quote and the reverse.

| Kind | Producer | Evidence | Verifier |
|---|---|---|---|
| `secure-enclave` | macOS, via the `playbox-se` helper (`tools/playbox-se/build.sh`) | P-256 signature over the statement's binding | `SecureEnclaveVerifier` |
| `tpm` | Linux, via `tpm2-tools` (`tpm2_createek`, `tpm2_createak`, `tpm2_quote`, `tpm2_pcrread`) | A TPM 2.0 quote (ECDSA P-256) over PCRs 0-7 and 23, with the PCR values | `TpmVerifier` |

**Binding.** Evidence is tied to a statement through `Statement::binding()`, a digest of everything in the
statement except the evidence itself and the claimed tier. The Secure Enclave signs it; the TPM puts it in
the quote's qualifying data. So evidence cannot be moved to another subject, nonce, history position,
measurement set or platform kind. Tests lift a genuine quote into another statement and it is rejected.

**Pinning decides trust.** A valid signature or quote shows that *some* key signed this statement. It does
not show the key belongs to genuine hardware: a software TPM or an ordinary P-256 key produces the same
bytes. So a verifier reaches T1 only when the device key is in its pin store (`~/.playbox/pins`, lines of
`<kind> <fingerprint>`). Unpinned evidence is reported with its fingerprint and stays T0.

```sh
playbox attest --platform se       # or tpm; add hardware evidence to a history log
playbox verify <log>               # shows each device fingerprint and how to pin it
playbox pin add secure-enclave <fingerprint>
playbox pin list | remove <kind> <fingerprint>
```

`playbox_join` does the same for a room host: `min_host_tier: "t1"` refuses a host whose device key you have
not pinned, and `trust_host_device: true` pins it on first use. Only use trust-on-first-use when you have good
reason to think you reached the host you meant; first contact is the one an impostor could win. The pin
store is keyed per device key, so one pin covers every later statement from that device.

**T2** needs more: pinned TPM key, PCR 23 equal to a single extend of the host's declared measurements
(the host resets PCR 23 and extends it before each quote), *and* operator-supplied reference values that
match for at least one boot PCR 0-7 (`PLAYBOX_TPM_BOOT_PCRS=0:<hex>,7:<hex>`). Without reference boot values,
a quote cannot tell an honest host from one that extended whatever it wanted. T3 (confidential VMs) has no
verifier yet.

**Secure Enclave details.** The helper uses CryptoKit, which keeps the key as an opaque handle in a file
(`~/.playbox/se.key`, mode 0600) usable only on this Mac's Secure Enclave. Keychain-stored Secure Enclave keys
were tried first and fail in a plain command-line tool with `errSecMissingEntitlement` (-34018).

### What is and is not proven

- Proven: the key that signed the statement also holds the device key (the device signed a binding that
  includes the statement's subject), the evidence answers this nonce, and the quote's PCR values are
  consistent with the TPM's signature.
- Not proven by default: that the device is genuine. For a TPM, set `PLAYBOX_TPM_EK_ANCHORS` (below) to prove it.
  Apple App Attest for the Secure Enclave needs an app bundle and Apple developer entitlements and is not
  implemented, so Secure Enclave trust comes from the pin alone.
- Not proven: that a root-compromised host is honest. It can extend PCR 23 with anything. Boot PCR
  reference values (T2) narrow this to hosts whose firmware and boot chain match what you expect, but a
  later compromise of the running system is not covered; that needs IMA or a confidential VM.

### How this was tested

- Secure Enclave: a real key on an Apple M4 Max, through the helper, in `tests/secure_enclave.rs`, in
  `playbox-net/tests/attest.rs` (host and guest over iroh, unpinned T0 then pinned T1) and in
  `playbox-mcp/tests/platform.rs` (trust on first use, pin persisted across a restart).
- TPM: real quotes from a **software** TPM (swtpm 0.10.1) on Ubuntu 26.04 aarch64 in a Lima VM, committed as
  `playbox-core/tests/fixtures/swtpm-statement-{1,2}.txt` and verified on macOS in `tests/tpm_fixtures.rs`.
  The tests check that the TPM's own PCR 23 equals our computation of the extend, that two statements
  share one persistent attestation key, that a genuine quote cannot be replayed into another statement,
  and that corrupting a PCR value or the quote is detected.
- No hardware TPM, no Secure Boot values, and no confidential VM have been tested.

To regenerate the TPM fixtures: in a Linux machine or Lima VM with `swtpm`, `swtpm-tools`, `tpm2-tools` and
Rust, start a fresh software TPM (`swtpm_setup --tpm2 --tpmstate DIR --createek --pcr-banks sha256`, then
`swtpm socket --tpm2 --tpmstate dir=DIR --server type=tcp,port=2321 --ctrl type=tcp,port=2322 --flags
startup-clear --daemon`), set `TPM2TOOLS_TCTI=swtpm:host=127.0.0.1,port=2321`, and run
`cargo run --release -p playbox-core --example platform_attest -- tpm <nonce>` twice with different
nonces. On a real Linux host, drop the TCTI variable, make sure the user can open `/dev/tpmrm0` (group
`tss`), and the same command produces real evidence. A TPM without a resource manager needs the provider's
`tpm2_flushcontext -t` calls, which it makes itself.

## Proving a TPM is genuine (EK certificate and credential activation)

A quote shows that some attestation key (AK) signed; a software TPM looks identical. To show the AK lives
in a chip a manufacturer certified, the verifier runs the TPM's own protocol. Set
`PLAYBOX_TPM_EK_ANCHORS=<PEM file>` to the root or intermediate certificates you trust to vouch for TPM
endorsement keys (EKs). Then:

1. The AK must be a **restricted, non-exportable signing key** (`fixedTPM`, `fixedParent`, `restricted`,
   `sign`, not `decrypt`). This is checked even without anchors; a quote under any other key proves nothing,
   since an unrestricted key can sign bytes that merely look like a quote.
2. The host sends its EK certificate (read from the TPM's NV index; intermediates from the host's
   `PLAYBOX_TPM_EK_CHAIN` PEM). It must chain to one of your anchors, be valid now, and have CA
   intermediates. Revocation is not checked.
3. The verifier encrypts a fresh random secret to the EK's public key, bound to the AK's name
   (`TPM2_MakeCredential`, done in software, RSA and ECC EKs both supported) and sends it as
   `playbox.ActivateCredential`.
4. Only a TPM holding that EK **and** that AK can decrypt it (`TPM2_ActivateCredential`). The returned secret
   is compared in constant time. Until it matches, the tier stays T0, because the certificate is public and
   anyone could replay it.

With anchors configured, a pinned host that cannot prove this (no certificate, wrong chain, failed
activation) stays at T0; without anchors the old pin-only behaviour applies and the output says genuineness
was not checked. A saved history cannot repeat the live challenge, so `playbox verify` on a log shows the
tier without it.

Tested: `make_credential` against a real TPM implementation's `ActivateCredential` (RSA and ECC, plus
wrong-AK-name, flipped-bit and wrong-EK refusals), in `playbox-core/tests/tpm_hardware.rs` (run in a VM with
`PLAYBOX_TEST_TPM=1`); the certificate chain offline against fixtures (`tests/tpm_fixtures.rs`); and the
whole path across machines: a Linux host with swtpm serving over iroh's public relays, a Mac guest with the
swtpm CA as anchor reaching T1 after the TPM answered, and the wrong anchor staying at T0. The "manufacturer" there is
swtpm's own CA, so this shows the mechanism works, not that any real vendor's chain verifies. A hardware
TPM (Intel PTT, AMD fTPM, Infineon, ...) has not been tried; real vendors' intermediates are usually fetched
from their websites, so expect to supply them via `PLAYBOX_TPM_EK_CHAIN` and anchors.

## Policy

`attest::Policy` maps an operation to a minimum tier (`Policy::default().require("merge", Tier::T2)`).
It is not enforced by the host yet; with only T0 available, any requirement above T0 can't be met.
