# Decisions

| # | Decision | Why | Revisit when |
|---|---|---|---|
| 1 | iroh over rust-libp2p | QUIC-native, dial-by-key, relay fallback, 1.x stable (v1.4.1) | We need gossipsub/Kademlia semantics beyond iroh-gossip |
| 2 | Biscuit over UCAN | Datalog policy, offline attenuation, mature Rust crate; UCAN 1.0 still rc | UCAN 1.0 ratified and we want DID-native chains |
| 3 | Plain git, bundles over iroh-blobs | jj and gitoxide are pre-1.0 | Bundle sync is a bottleneck |
| 4 | Host-centered room, no global consensus | Simpler trust model; members mirror the log to detect tampering | Multi-host rooms |
| 5 | Lean models grant logic only, trusts crypto | Days not months; follows the Cedar approach | Token format is fixed |
| 6 | Pin libkrun to a release (v1.19.x) | 2.0 removes deprecated APIs | 2.0 released |
| 7 | M2 runner is Docker for now; libkrun runner is a refusing placeholder | libkrun is not installed on the dev Mac; container isolation is weaker than a microVM | A Linux/KVM or HVF-capable libkrun setup is available |
| 8 | Pushes land in per-member refs, never on `main` | Merging into `main` is a separate, later-gated act | M3 grants define who may merge |
| 9 | Grants bound to the member's iroh key via a `subject` check | A stolen grant is useless to any other key | Hosts that proxy for members |
| 10 | Over-limit task requests are refused, not clamped | Members learn their limits; the grant is the single source of truth | Never |
| 11 | Revocation is host-local and in memory; revoking an authority block id kills all attenuations | Single-host rooms need no gossip yet | Multi-host rooms or restarts that must keep bans (persist + gossip over iroh-gossip) |
| 12 | Hand-rolled MCP over stdio instead of an SDK | The surface is three methods; avoids tracking an SDK that moves fast | We need resources, prompts, or the HTTP transport |
| 13 | The unsafe no-isolation runner needs an env var on the host; agents cannot enable it by argument alone | A prompt-injected agent must not be able to switch off the sandbox | Never |
| 14 | History is a text file, one signed entry per line, locked on append | Greppable, diffable, no extra dependency | Logs get large enough to need an index |
| 15 | Transport is a host choice recorded in the join link; joiners follow it | A guest should not need to know how the host is reachable | A joiner must use a different path (override exists) |
| 16 | Hosts persist identity, root key, invites, members and bans in a state dir; saves are best effort and logged | Rooms must survive restarts | Multi-host rooms need replicated revocation |
| 17 | In `local` mode the host also advertises loopback | iroh lists only the LAN address, so same-machine peers looped through the Wi-Fi interface and intermittently timed out under the macOS firewall (0 of 8 full runs failed after the change; about 1 in 6 failed before) | Never; LAN peers still use the LAN address |
| 18 | Client connect retries 4 times at 6s each | Cheap insurance; did not fix the loopback issue by itself | If retries hide real faults |
| 19 | A2A over iroh streams is a custom binding (`PLAYBOX-IROH`): JSON-RPC plus service parameters in a frame, grant as a Bearer service parameter | A2A defines HTTP, gRPC and similar bindings only; iroh gives identity and NAT traversal | A2A standardizes a stream binding, or we add an HTTP bridge |
| 20 | Use only the `a2a-lf` types, not its axum server or HTTP client | They assume HTTP; the room needs its own auth and transport | The SDK grows a transport-agnostic server |
| 21 | Join and the public card are extra methods (`playbox.Join`, `playbox.GetAgentCard`) | A2A has no join flow and the card normally lives at a URL | Never |
| 22 | Streaming is unsupported; tasks finish before they return | Keeps the host simple; long tasks block the call (bounded by the grant's timeout) | Tasks that outlive one call |
| 23 | The HTTP bridge is one room member, not a per-caller proxy; callers share its identity, grant and tasks | Per-caller identity would need a way to mint grants over HTTP, i.e. a second auth system | Multi-tenant bridge with per-caller invites |
| 24 | Bridge refuses non-loopback listening without a bearer token | An open bridge hands the member's powers to anyone on the network | Never |
| 25 | Attestation statements are self-signed text; the achieved tier is separate from the claimed tier and only platform verifiers can raise it | Claiming a tier must not be the same as having it | A platform verifier exists |
| 26 | Hosts sign attestations with the iroh endpoint key, so clients bind them to the connection they already authenticated | No second identity to manage or confuse | Hardware-bound keys |
| 27 | Guests verify the host before presenting the invite | A host that fails the check never learns the invite and the single-use link is not spent | Never |
| 28 | Only attestation bodies are kept beside the log; ordinary entries stay hash-only | Keeps logs small and prompts out of them | Verification needs other bodies |
| 29 | Hardware evidence only reaches T1 when the device key is pinned by the verifier | A software TPM or any P-256 key produces the same bytes as real hardware, so a valid quote alone proves nothing about genuineness | EK certificate chains (TPM) or App Attest (Apple) are checked |
| 30 | Binding = digest of the statement minus the evidence and the claimed tier; hardware signs or quotes it | Evidence cannot be moved to another subject, nonce, history position or measurement set; the claim is only a cap | Never |
| 31 | The Secure Enclave helper uses CryptoKit key handles, not the keychain | Keychain-stored Secure Enclave keys fail in a plain CLI with errSecMissingEntitlement (-34018); CryptoKit handles work and reload across runs | The tool ships as a signed app with entitlements |
| 32 | T2 needs operator-supplied boot PCR reference values | Without them a quote cannot tell an honest host from one that extended arbitrary values into PCR 23 | IMA or secure-boot policy values can be derived automatically |
| 33 | TPM verification is pure Rust (p256, sha2) and the TPM is driven through `tpm2-tools` | Verification runs on any OS; avoids a TSS library dependency and build on Mac | A native TSS binding is needed for speed or features |
| 34 | A provider failure at request time yields a plain T0 statement and a log line | One broken TPM must not take the room down; clients that need T1 still refuse | The room should refuse to start without working hardware |
| 29 | The libkrun runner spawns a helper process per task | `krun_start_enter` never returns; one process per VM is the only safe shape | libkrun gains a returning start call |
| 30 | Network is off by removing the vsock device, and `find_helper` prefers the installed signed helper over a freshly built one | Verified: with the device gone the guest has no route; an unsigned helper fails with -22 on macOS | A filtered network backend replaces TSI |
| 31 | Root filesystems are exported from Docker and cloned per task | Reuses images people already have; clonefile makes a per-task root nearly free | Images are pulled and unpacked natively |
| 32 | Revocation is a root-signed notice that any host accepts from any source | Authenticity comes from the signature, so gossip and pull need no trust in the carrier | Delegated revokers (a second signing key) |
| 33 | Gossip for speed plus periodic pull for repair, over a grow-only set | Gossip alone loses messages to offline hosts; a grow-only set makes order and duplicates harmless | Notice volume makes full-list pulls too big |
| 34 | A host serves notices only to configured sync peers | Notices name revoked members' endpoint ids; strangers need not learn them | A public revocation list is wanted |
| 35 | Replicas hold only the root public key | A replica compromise cannot admit members or revoke | Replicas that must admit members |
| 35 | The AK must be restricted and fixed, checked from its TPMT_PUBLIC, even without EK anchors | An unrestricted key can sign bytes that look like a quote; the earlier raw-key check missed this | Never |
| 36 | Genuineness is opt-in via `PLAYBOX_TPM_EK_ANCHORS`; with anchors, failing it holds a host at T0 | We cannot ship manufacturer roots we cannot vouch for; operators choose whom to trust | A vetted default root set |
| 37 | Interactive proof (credential activation) lives in `Verified.challenge`; offline checks keep the lower tier | The EK certificate is public, so a static check alone proves nothing | Never |
| 38 | MakeCredential is implemented in Rust and validated against a real TPM, not against our own reading of the spec | Layouts and KDF details are easy to get subtly wrong | A TSS binding replaces it |
| 36 | The Docker runner fails closed at start when the daemon cannot enforce memory, process or CPU limits | Found on a Raspberry Pi: Docker warned and dropped the memory limit, so a grant's cap was fiction | A mode that reports the missing limit to members instead |
| 37 | After a task, git runs only with an explicit `--git-dir` recorded before the task, never against whatever `.git` the task left | A task replaced `.git` and got the host to run its commands (sandbox escape); the directory a task wrote to can name nothing the host trusts | Never |
| 38 | Before a connection proves itself it may send 64 KiB, must be quick, and counts against connection caps | Anyone with an iroh key can dial; the host used to buffer 256 MiB before checking a grant | A separate, cheaper pre-auth handshake |
| 39 | The no-token HTTP bridge refuses any request with an `Origin`, any non-loopback `Host`, and any non-JSON content type | A web page could drive a localhost bridge (cross-site POST, DNS rebinding) | Bridge tokens are mandatory |
| 40 | The audit log is hash-chained and signed by the host key, never records grants, invites or command arguments, and the host refuses to write into a log that fails the check | A log an attacker can quietly edit is worse than none; arguments hold secrets | An external log sink or transparency log |
| 41 | The task disk quota is a polling watchdog that measures *growth* and kills the sandbox | There is no portable filesystem quota for a bind-mounted directory; growth means a big checkout does not count | Per-task size-limited volumes |

Open: interoperate with Peerspan (peerspan/peerspan, peerfs, runtime) or stay independent.

## Workspace layer: adopt LayerFS ideas, not LayerFS (2026-10-28)

Source: Ephemeral-AI-Lab/layerfs v1.1.0 (MIT, JS/native, SQLite store, FUSE, Disposable durability, no signing, Linux mount tests waived in 1.1.0).

Adopted into `playbox-core::workspace`:
- A commit per tool call, recorded with the tool-call id; the commit's tree id is the `fs_snapshot` payload in the signed history log.
- Free fork: a branch is a pointer.
- Content-defined chunking plus content-addressed objects: an edit costs the edit, identical content is stored once.
- Refuse on conflict when both sides changed the same path; `.lww` rules (exact, `*.ext`, `dir/`) let the newest edit win.
- Append-only objects, verified on every read.

Not adopted: the SQLite store, FUSE mounting, its JS SDK and worker, and its Disposable (non-durable) profile. We add what it lacks: signed history, grants and attestation.

Revisit: if a Linux spike of LayerFS beats our module on mount speed, run it behind the same `Workspace` interface. Compare with Peerspan's `peerfs`, which already signs commits.

## OpenShell runner (2026-10-28)
`--runtime openshell` runs each task in an NVIDIA OpenShell sandbox via its CLI (`crates/playbox-runner/src/openshell.rs`). Reported isolation: `Container`. Per-task policy: Landlock read-only system paths, `/workspace` and `/tmp` writable, non-root, no network rules (default deny). Fail-closed choices: OpenShell policies have no CPU/memory/timeout limits and its VM driver ignores `--cpu/--memory`, so `preflight` refuses unless `PLAYBOX_ALLOW_MISSING_LIMITS=1`; tasks asking for network are refused (no per-host allow-list in grants yet). Tested for real against OpenShell 0.1.6 (macOS, local gateway, VM driver): round trip, exit code, stderr, non-root user, read-only system paths, no network, timeout. Files move by piping tar through `exec` (`create --upload` cannot take a command, nests directories, and `download` needs SSH). Sandbox names max 19 chars. Needs `brew install e2fsprogs` and a gateway with an auth config and the VM driver dir set (see the scratchpad gw.toml approach). Next: run it for real, record `openshell_policy_yaml()` digest and OpenShell version in the signed host statement.
