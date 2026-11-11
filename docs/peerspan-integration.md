# What to take from Peerspan, and how, without losing connect-and-verify

Status (2026-11-11). A read of `peerspan/peerspan` (Rust host, docs, integrations), `peerspan/runtime`, `peerspan/peerfs` at their
main branches. Not read: `peerspan-go` (the second host implementation) beyond its listing, `libkrun` (a thin fork), and anything I
could not infer from code and contracts. Nothing here was built or run; Peerspan's code was not executed. Both projects are
Apache-2.0, so code can be adopted with its SPDX header and a NOTICE line.

## The rule every import must pass

Playbox's claim is *connect and verify*: a stranger can join, run bounded work in a sandbox, and the other side can check what
happened. An import is accepted only if it keeps all four:

1. **Nothing foreign runs outside the runner.** Any request from a peer that executes something goes through `Runner`, and the host
   refuses to start below `min_isolation`.
2. **Every new operation is gated by a Biscuit grant**, goes through `audit_op` into the signed log, has size and rate bounds, a
   fuzz seed in `fuzz_host.rs` and a libFuzzer target if it parses bytes.
3. **Anything a peer can read about this host is measured.** New confinement settings go into the attestation measurements, so a
   guest can see "seccomp on, network level internet, cgroup limits" and refuse a host that lacks them.
4. **Peer-written text is untrusted.** Anything that pushes peer text toward an agent (conversations, channel events) is tainted by the
   witness plugin the same way `feed` is.

## 1. Sandbox hardening from `peerspan/runtime` (highest value, Linux)

Status 2026-11-11: `jail`, `cgroup` and `netpolicy` are built and tested in `playbox-runner` (see `docs/libkrun.md`, "Linux
confinement"); not yet wired into the helper; seccomp, secrets, disk I/O limits, passt and the attestation measurements remain.

Playbox's libkrun runner (`docs/libkrun.md`) says what it lacks: with the network on, libkrun's TSI gives the guest the host's
loopback and LAN with no filtering; no pid limit; no disk quota; the helper process runs with the host user's full authority; it has
never run on Linux/KVM. The runtime fixes each of these on Linux, and its launcher has the same shape as `playbox-krun-helper` (one
short-lived process per task that confines itself, then boots the VM). Take, in this order:

| Peerspan file | What it does | Playbox target |
|---|---|---|
| `crates/runtime/src/jail.rs` | New mount, PID, net, IPC, UTS namespaces; pivot into a root that holds only rootfs, kernel and sockets; drop to the run's uid; no-new-privs; refuses to boot unconfined if any step fails | `playbox-runner/src/linux/jail.rs`, called by `playbox-krun-helper` before `krun_start_enter` |
| `seccomp.rs` | Allow-list of the syscalls libkrun makes, installed before libkrun starts a thread | same dir; **re-derive the list** against our libkrun and our `check` runs: theirs was measured on their fork |
| `cgroup.rs` | Per-run cgroup v2: memory, cpu, pids (512), disk I/O; flock-based stale cleanup if the supervisor dies | same dir; closes the "no pid limit, no disk quota" rows |
| `net.rs` | passt per run plus an nftables chain per cgroup, fails closed; levels `none`, `internet`, `internet+lan`; always rejects loopback, link-local (cloud metadata 169.254.169.254), multicast, NAT64 aliases of those | replaces the TSI path. Keep Playbox's per-host allowlist (`TaskSpec.hosts`) on top, which is finer than Peerspan's three levels |
| `userns.rs` | maps the workspace server's root to one host user | only if we move to virtiofsd on Linux; skip otherwise |
| protocol `Secret` (`/run/secrets`, 16 max, 64 KiB, root-only tmpfs, outside the workspace) | credentials reach a task without entering the workspace or the returned patch | add `TaskSpec.secrets`; audit log records names and hashes, never values |
| `doctor.rs` | checks everything a run needs and says what to fix | `playbox doctor` |

What this buys in the verify story, which Peerspan does not have: put `jail`, `seccomp`, `cgroup`, `net_level` and the guest
kernel and image hashes into `attest::measure_self`. Today the doc says a microVM "shows isolation, not attestation"; this is the
first step to tier evidence that covers the guest.

Costs and risks: Linux and root (or a privileged helper) only, so macOS keeps the current runner and the doc must say so; Peerspan's
own README calls the runtime "early development; not usable yet"; the seccomp list will break on libkrun upgrades unless it is run
in CI. Validate on a gman node, not a Mac: the Linux/KVM path has never run.

## 2. Durable messaging with request-ID dedup (`src/adapter.rs`, `peer` contract)

Playbox's chat is memory-only and a lost response is unrecoverable; the latent mailbox (6l) is the only persisted path. Peerspan's
design, which is sound and worth copying exactly:

- A sender mints a `request_id`; the receiver stores `UNIQUE(sender, request_id)` and an identical retry returns the original receipt.
  The request body is part of the identity (even `origin`), so a different body under the same id is rejected, not merged.
- Outbound records carry a state: `accepted`, `rejected` (final) or `unknown`. Retry schedule: first attempt due 10 s after
  preparation, then 1 s after an `unknown`, doubling to 60 s with ±20% jitter, at most 4 attempts in flight, a restart spreads overdue
  records over 10 s. After the deadline only the receipt is queried (10 queries or 24 h), because resubmitting would be refused.
- One bounded retry on a fresh connection if a reused connection fails before any response.

Playbox adds what theirs lacks: make the receipt a **host-signed** statement (generalise `playbox.latent-receipt.v1`), so an
`unknown` resolves to something a third party can check, and chain it into the audit log. I did not verify whether Peerspan's
receipts are signed.

Storage: they use SQLite (`rusqlite`) with a table per concern. Playbox keeps state in JSON files. For dedup a transaction is
worth a dependency: add a `store` module with `rusqlite` for messages, tasks and calls only, and leave identity, grants and the audit
log where they are.

## 3. Shared capabilities with per-call approval (`calls.rs`, `approvals.rs`, `bounds.rs`, `canonical.rs`)

An owner exposes chosen operations of their own MCP tools to peers. Take the model, not the code:
a call is authorised by policy, persisted before it runs, run exactly once, and polled by id; bounds limit named arguments to listed
combinations and are checked at submission and again before start; an owner can require per-call approval at a terminal.

In Playbox this maps onto existing parts: the grant is a Biscuit caveat (`capability("name")`), bounds become Biscuit checks on the
arguments (a stronger base than Peerspan's list, since scope can only narrow), approval reuses the human-approval flow in
`playbox-mcp`. Where we improve on them: **run the capability server inside the runner** when the owner opts in, so a peer's call
does not execute with the owner's full authority; and hash the result with Peerspan's canonical JSON encoding (`canonical.rs`: sorted
keys, numbers as received, minimal escapes) into the audit log, so the result is verifiable.

## 4. Conversations (`conversations.rs` and `conversations/*`, 242 KB + 120 KB)

Multi-party threads with a home host, a roster version, per-author sequence and clock, invitations, read marks, and per-author rate
limits. Port the **data model and a first slice only**: home-host ordered log, roster, `since` cursor, read marks, rate limit.
Do not port the 242 KB module. Playbox differences: entries signed by the member key and anchored in the audit log, and a message
may carry a latent-vector attachment (section 6f). Entries are peer text: treat as untrusted (rule 4).

## 5. Live delivery into the agent (`integrations/claude-channel`, live-delivery contract)

Peerspan pushes incoming messages into a running Claude Code session (a channel), with a nonce handshake (`hello` event, the model
must call `peerspan_ready` with the nonce before any work is delivered), a `doctor` that checks the Claude version, account type and
channel state, and a "connected" flag that is true only after verification. Playbox's agents poll `feed`. Take the handshake and the
doctor checks. This is also the strongest prompt-injection path in the product, so it must land together with rule 4: the witness
plugin has to treat channel events as peer text.

## 6. Conformance suite (`conformance/`, ~500 KB Python)

Black-box scenarios that drive *installed* binaries, in a per-push tier and a full tier. Playbox's tests are in-process Rust plus
`ui/e2e.py`. Create `conformance/` with scenarios that start real `playbox up` and `playbox mcp`: join, revoke, push/pull, task in
each runner, latent post/restart (`steer/tools/e8_room_hardening.py` is the first scenario), message retry across a host restart.
Keep the two tiers: the macOS CI job that keeps being cancelled should run only the fast tier.

## 7. PeerFS ideas worth borrowing (not the store)

Playbox's workspace is real git; do not adopt a content-addressed store. Borrow: **path classes** (source / derived / secret) so a
returned patch never includes secrets or build output; a signed snapshot per tool call (their `PostToolUse` hook) so a run's
history is attributable to host, agent and session; commits refused unless signed by a key listed for the author host.

## Not worth copying

- The machine-linking and onboarding ceremonies (`src/link_runtime`, `src/decentralized`, about 2 MB of Rust and over a hundred
  contract documents). iroh and Biscuit already do the connecting; Peerspan's own roadmap shows it retired a coordinator product.
- Delegation chains (`chain.rs`): host-signed links that only narrow. Biscuit attenuation gives the same property with a standard
  token and a shorter implementation.
- Node-based adapters (Node 22 was blocking their installer; their 0.2.1 plan bundles Node). Playbox's adapters are one Rust MCP server.
- The documentation style: single contract files of 200-680 KB cannot be reviewed.

## Order

1. Sandbox hardening (section 1) with the attestation measurements. It is the only item that makes the *verify* half stronger, and
   it needs a Linux box (gman).
2. Durable messaging with signed receipts (section 2). Fixes the real data-loss gap and is a prerequisite for 3 and 4.
3. Conformance harness (section 6), started with items 1 and 2 as its first scenarios.
4. Capabilities (section 3), then conversations and live delivery (4, 5) together with the witness change.
