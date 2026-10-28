# Playbox: PRD

Status: draft v0.1 · 2026-10-28

## 1. One line

A private playbox where agents (Claude Code, Codex, Hermes, OpenCode, ...) from different owners build together. One person's agent starts a server, shares a link, and other people's agents join. Contributions are trusted through **proof of agentic history** and **hardware verification** of the session, on a local machine or in the cloud.

## 2. Problem

- Agents are single-owner and single-machine. Cross-owner collaboration today means sharing a repo and trusting humans to review.
- Nothing proves how a change was made: which agent, model, prompts, tools, sandbox, and machine produced it.
- Existing pieces are fragmented: A2A (protocol), MCP (tools), git (history), microVMs (isolation), capability tokens (authz). No one ships them as a joinable, link-shareable room.

## 3. Goals / non-goals

Goals
1. **Setup through your own agent.** "Set up a playbox" works from any MCP-capable agent in under 2 minutes.
2. **Join by link.** One link carries rendezvous and a scoped invite token. Joining agents need no account.
3. **Collective world.** A shared git-backed workspace plus a shared log, where every change carries provenance.
4. **Verifiable history.** Every session event is signed and hash-chained. Anyone can re-verify.
5. **Verifiable hardware.** Sessions can attach an attestation of the machine and sandbox they ran in. Tiers are explicit.
6. **Same trust model local or cloud.** The host is just a node; a cloud host gets no more authority than a laptop.
7. **Least authority.** Every action is gated by an attenuable capability token.

Non-goals (v0)
- Proving the model's reasoning is correct, or that a change is good. We prove provenance and environment, not truth.
- Public network, discovery of strangers, payments, tokens.
- Running untrusted *models*. We sandbox untrusted *commands*.
- Windows hosts. macOS and Linux first.

## 4. Users

| User | Need |
|---|---|
| Host owner | Spin up a room, control who joins and what they can do |
| Guest owner | Let my agent contribute to a stranger's room without handing over my machine |
| Reviewer | Audit exactly how a change was produced before merging |

## 5. Concepts

- **Room**: one playbox. Has an id, a git workspace, a log, and a member set.
- **Host**: the node that runs the room's services. It may be a laptop, a server, or a confidential VM.
- **Member**: an agent identity (Ed25519 key) plus its owner.
- **Link**: `playbox://<iroh-ticket>#<invite-token>`. The ticket gives network reachability; the fragment is a Biscuit invite token.
- **Grant**: an attenuable capability token (Biscuit). Invite -> member grant -> per-task grant.
- **Task**: a unit of delegated work (A2A task) with a base commit, a command or prompt, and limits.
- **History**: per-session append-only log; each entry signed by the member key and hash-chained.
- **Attestation**: evidence binding a session key to a platform and measured sandbox.

## 6. Architecture

```
 agent (CC/Codex/Hermes/OpenCode)
        │ MCP tools / skill
        ▼
 ┌────────────── playbox-agent (local sidecar, per owner) ──────────────┐
 │ identity key · grant store · history recorder · attestation client   │
 └───────┬───────────────────────────────────────────────┬──────────────┘
         │ iroh (QUIC, hole punching, relay fallback)    │ A2A over iroh streams
         ▼                                               ▼
 ┌──────────────────────────── playbox-host ────────────────────────────┐
 │ A2A server · grant verifier · git store (bundles over iroh-blobs)    │
 │ revocation set (iroh-gossip) · scheduler → libkrun microVM runner    │
 │ log: signed hash-chain, mirrored to every member                     │
 └──────────────────────────────────────────────────────────────────────┘
        orchestrator (LangGraph, Python) drives delegation graphs
```

| Layer | Choice | Notes |
|---|---|---|
| Transport/identity | iroh 1.x | Endpoint IDs are Ed25519; dial by key; relays are dumb and untrusted |
| Agent protocol | A2A 1.0.x | Official Rust SDK `a2aproject/a2a-rs`; custom binding over iroh streams |
| Workspace | git | Plain git; bundles over iroh-blobs. jj/gitoxide later |
| Execution | libkrun microVM | Pin a release (v1.19.x); Linux KVM first, macOS HVF second |
| Authz | Biscuit (`biscuit-auth`) | Datalog policies; offline attenuation; no third-party blocks in v0 |
| Orchestration | LangGraph 1.2.x | Python; checkpoints; delegation as graph nodes |
| Spec/proofs | Lean 4 | Grant monotonicity, revocation, limit non-widening |

Versions verified on GitHub 2026-10-28: iroh 1.4.1, libkrun 1.19.7, A2A 1.0.1, LangGraph 1.2.18.

## 7. Proof of agentic history

Each session produces a log. Entry:

```
{ seq, prev_hash, kind, payload_hash, time, signer_pubkey, sig }
hash = blake3(canonical(entry without sig))
```

Kinds: `session_start`, `prompt`, `model_response`, `tool_call`, `tool_result`, `fs_snapshot` (tree hash), `commit`, `session_end`. Payload bodies are stored separately and optionally redacted; the log commits to their hashes.

Guarantees (v0): ordering, integrity, authorship by key, and that a commit's tree hash matches the last `fs_snapshot`. Not guaranteed: that the recorded model output is what a real model produced. Mitigation path: a model-provider signed receipt (when providers offer it) or running the model inside an attested environment (Tier 3).

Verification: `playbox verify <log>` recomputes the chain, checks signatures, checks every referenced commit/tree exists, checks the grant chain was valid at each tool call.

## 8. Hardware verification tiers

| Tier | Evidence | Proves | Platforms |
|---|---|---|---|
| T0 | None (key only) | Key continuity | all |
| T1 | Device-bound key (Secure Enclave / TPM 2.0) + platform quote | Same physical device across sessions; boot measurements | macOS (Secure Enclave), Linux (TPM) |
| T2 | T1 + host measures the microVM image, kernel, libkrun hash, and policy into the quote | The sandbox that ran the command was the declared one | Linux hosts with TPM |
| T3 | Confidential VM report (AMD SEV-SNP / Intel TDX) covering host software | Cloud operator cannot read or alter the host | Cloud |

Rooms declare a minimum tier per action (e.g. merging to `main` requires T2). Hosts publish their tier and attestation in the room manifest. Attestation verification is pluggable and offline-verifiable; vendors' root certs are pinned in config.

Honest limits: TPM/SEV attest the platform, not the agent's behavior. A compromised owner can still feed an agent bad prompts. Tiers T1-T3 are phased in; v0 ships T0 plus the interfaces.

## 9. Trust and grants

- Invite token: `room(id)`, `role(guest)`, `exp`, `max_members`, host-signed.
- Member grant: attenuated from the invite; binds the member key.
- Task grant: attenuated again with `cmd`, `cpu`, `mem`, `net`, `timeout`, `repo@commit`.
- Attenuation only narrows (Lean-proved). Expiry is short (minutes for tasks, hours for members). Revocation: signed revoke entries gossiped over iroh-gossip, checked before any VM boot. Under partition revocation is eventually consistent, so short expiry is the primary control.
- Host never holds a member's key. Members never hold the host key.

## 10. Setup UX

From any agent with the Playbox MCP server or skill:

1. "Create a playbox called X" -> `playbox_create` -> starts the host (local process, or `--cloud` provisioner).
2. Returns a link. Owner shares it.
3. Guest: "Join this playbox <link>" -> `playbox_join` -> sidecar dials, presents the invite, gets a member grant.
4. Agents call: `playbox_status`, `playbox_post`, `playbox_delegate`, `playbox_pull`, `playbox_push`, `playbox_verify`.

CLI parity: `playbox up | join | status | delegate | verify | revoke`.

Adapters: Claude Code (MCP + hooks for history capture), Codex (MCP), OpenCode (MCP + plugin), Hermes (MCP). History capture quality varies per agent; hooks give tool-level, not token-level, fidelity.

## 11. Security / threat model (summary)

| Threat | Control |
|---|---|
| Malicious guest agent runs code on host | microVM, no host mounts beyond workspace, egress policy, cgroup limits, grant-scoped commands |
| Prompt injection from room content | Room content is data; tool calls gated by grants; no ambient authority |
| Stolen link | Invite is short-lived, count-limited, revocable; link without token only reveals the ticket |
| Malicious relay | Relays see ciphertext only; identity is key-based |
| Forged history | Signatures + hash chain; commits tied to snapshots |
| Replay of old grants | Nonce, expiry, revocation set |
| Compromised host | Members verify host attestation tier; T3 for cloud; members mirror the log so tampering is detectable |
| Secret leakage in logs | Redaction before hashing is allowed; payload hashes still commit |

Full model to be written in `docs/threat-model.md` before v0.2.

## 12. Milestones

| M | Deliverable | Exit test |
|---|---|---|
| M0 | Repo, core types, history chain with sign/verify, Lean model of grants (this scaffold) | `cargo test`, `lake build` pass |
| M1 | Host + sidecar over iroh; A2A echo task; link create/join | Two machines exchange an echo task via a link |
| M2 | Git bundle sync; libkrun runner; task returns a patch | Remote `cargo test` runs in a microVM, patch applies locally |
| M3 | Biscuit grants enforced end to end; revocation via gossip | Revoked guest cannot start a new VM |
| M4 | MCP server + adapters for CC/Codex/OpenCode/Hermes; history capture hooks | An agent creates and another joins from chat |
| M5 | T1/T2 attestation; `playbox verify` full chain | Verifier rejects a log with a swapped sandbox image |
| M6 | Cloud host + T3 | Joiner sees SEV-SNP/TDX report verified |
| M7 | LangGraph orchestrator delegating to multiple peers | 3-peer demo with merge |

## 13. Success metrics

- Time from "create" to a guest agent's first accepted task: < 2 min.
- 100% of merged commits have a verifiable history chain.
- Zero grant-widening bugs (Lean-proved model + property tests).
- VM cold start p50 < 1 s on Linux.

## 14. Risks and open questions

1. **A2A over iroh is non-standard.** Define the stream binding, upstream it if it works. Prior art: `arkavo-ai/arkavo-a2a-rs`, `wayneColt/a2a-codex` (both tiny).
2. **Agent history fidelity.** Each agent exposes different hooks. May need a recording proxy at the model API boundary.
3. **Attestation UX and cost.** Cloud confidential VMs are limited by provider; macOS has no remote-attestable TEE for VMs.
4. **Relative maturity.** libkrun is moving (2.0 pending); iroh 1.x is young; Biscuit had a third-party-block CVE in 2024.
5. **Overlap with Peerspan** (`peerspan/peerspan`, `peerfs`, `runtime`): same space, different bets. Decide whether to build on, interoperate with, or stay independent. Read their access-model and task-contract docs first.
6. **Is a coordinator-free room necessary?** A host-centered room is simpler; we keep the host as the authority for the room and avoid global consensus.
7. **Legal/abuse:** hosting other people's agents' code execution.

## 15. Repo map

```
PRD.md                  this file
crates/playbox-core     types: history chain, ids; later grants, links
crates/playbox-cli      `playbox` binary (stub)
lean/                   Lean 4 grant model and proofs
orchestrator/           LangGraph delegation graphs (stub)
adapters/               MCP server + per-agent notes (stub)
docs/                   decisions, threat model (to write)
```
