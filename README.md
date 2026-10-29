# playbox

A private playbox for collaborative agents. Start a room from your agent (Claude Code, Codex, Hermes, OpenCode), share a link, and other people's agents join and build together. Contributions are trusted through proof of agentic history and hardware verification, whether the host runs locally or in the cloud.

Status: M5 partial (Secure Enclave and TPM evidence with pin-based trust; no confidential-VM verifier), M4 done, M3 done, M2 partial, transports and persistent hosts done. Git bundle sync and sandboxed remote tasks work, in Docker containers or libkrun microVMs (macOS-tested; see [docs/libkrun.md](docs/libkrun.md)). Peers speak A2A v1 over iroh (see [docs/a2a-binding.md](docs/a2a-binding.md)). Tested across two real machines (a Mac and a Raspberry Pi 5): see [docs/tested-on.md](docs/tested-on.md). Security review and known gaps: [docs/threat-model.md](docs/threat-model.md). Hosts keep a signed, tamper-evident audit log (`playbox audit <file>`) and stop tasks that write past a disk quota. Read [PRD.md](PRD.md).

## What works today
- `playbox-core`: signed, hash-chained session history with verification (6 tests).
- `lean/`: Lean 4 model of grants with proofs that attenuation never widens authority, expired or revoked grants authorize nothing, and delegation chains stay within the root's limits.
- `playbox-net`: an iroh host that admits members by single-use, expiring invite links. Members can pull a branch as a git bundle, push a branch (stored under their own `refs/playbox/members/...`, never over `main`), and run a task on a checkout of any commit; the result comes back as a patch (18 tests incl. grants).
- **Grants (M3):** invites and member grants are Biscuit tokens signed by the host's root key (`playbox-net/src/grants.rs`). A grant is bound to the member's endpoint key, expires (default 1h), lists allowed operations and hard task limits (cpu, memory, timeout, network). Every request is authorized against facts the host supplies. Members can narrow their own grant offline (`Client::attenuate`). `Host::revoke_member` revokes the grant and every attenuation of it and bars the key from rejoining. Over-limit requests are refused, not clamped.
- **MCP server (M4):** `playbox-mcp` is a stdio MCP server with 9 tools (create, join, status, ping, pull, delegate, push, verify, revoke), tested end to end by driving two real server processes. It records every tool call into a signed history log. `playbox record` / `playbox verify` capture and check history from agent hooks. Config snippets for Claude Code, Codex, OpenCode and Hermes are in `adapters/` and are untested against the real agents. See [adapters/README.md](adapters/README.md).
- **Transports:** pick how peers reach each other with `--transport` (CLI), the `transport` argument (MCP) or `PLAYBOX_TRANSPORT`: `iroh` (n0's public relays and discovery, works across NATs, the default), `local` (same machine or LAN, no internet), or `relay:<https url>` (your own `iroh-relay`; set `PLAYBOX_RELAY_CA` to a PEM file for a private CA). The join link records the host's choice and joiners follow it unless they override. Tested with a real `iroh-relay` server where the host and guest can only meet through it. The public n0 relay was only checked with a host and guest on the same machine.
- **Persistent hosts:** `playbox up --state DIR` (or a named room in the MCP server) keeps the host's identity, signing key, unspent invites, members and bans on disk. After a restart the same grants still work and revoked members stay out.
- **A2A wire format:** the host is an A2A agent with a public agent card; operations are A2A messages, and pulls, pushes and runs are tracked A2A tasks whose results are artifacts (git bundle, stdout, stderr, patch). `GetTask` and `ListTasks` work per member. Built on the `a2a-lf` SDK types. `playbox bridge <link>` serves a room to stock HTTP A2A clients (tested with plain HTTP and curl).
- **Attestation (M5, partial):** hosts sign statements about their software; guests verify them against the authenticated connection key before joining (`min_host_tier`). Statements can carry hardware evidence: a Secure Enclave signature on macOS or a TPM 2.0 quote on Linux, verified on any OS. Hardware evidence reaches T1 only once the device key is pinned (`playbox pin`, or trust on first use at join); T2 also needs a TPM quote and boot PCR reference values. Tested with a real Secure Enclave and real quotes from a software TPM; no hardware TPM, no manufacturer certificate checks, no confidential-VM verifier. See [docs/attestation.md](docs/attestation.md).
- **Revocation across hosts:** a room can have replica hosts that know only the root public key. Revocations are root-signed notices, spread by iroh-gossip and repaired by periodic pulls, verified by every host, persisted, and unforgeable by peers. Tested with real replicas, including a replica that started after the revocation and one reached only by gossip. Guests can switch to a replica (`playbox_use_host`, `playbox join --via`). See [docs/revocation.md](docs/revocation.md).
- `playbox-git`: git CLI wrappers; peer-supplied ref names are validated before reaching git.
- `playbox-runner`: `DockerRunner` (no network, dropped capabilities, CPU/memory/pid/time limits, clamped by host caps), `ProcessRunner` (no isolation, dev only), `KrunRunner` (a libkrun microVM per task: own guest kernel, no network by default, copy-on-write root per task; needs `scripts/install-krun.sh`; select with `--runtime libkrun` or the MCP `runtime` argument).
- `playbox up | join <link> | demo`: add `--local` for LAN/offline use. Needs Rust 1.91+ (pinned to 1.93.1 in `rust-toolchain.toml`).

- **Trust layers:** an admission broker so text from other members cannot drive the host agent's actions without a person's single-use approval (`playbox approve`), a Google Confidential Space (`tdx`) verifier for T3, a signed evaluator with sealed holdout and variant lineage, shared memory with provenance and quarantine, model receipts, and witnessed log checkpoints. Only the broker is wired into the MCP server; the Confidential Space verifier is untested against a real VM. See [docs/trust-layers.md](docs/trust-layers.md).

- `world/`: prototype that learns a 3D scene from agent events (messages, actions, reasoning, commits). See [world/README.md](world/README.md).

## What does not exist yet
A2A binding, libkrun microVM runner, revocation gossip across hosts, persistent revocation, hardware attestation, attestation. See milestones M1-M7 in the PRD.

## Try it
```sh
cargo test
cargo run -p playbox-cli -- up --local      # prints a link
cargo run -p playbox-cli -- join '<link>' --local
cd lean && lake build
```

## Stack
iroh · A2A · git · libkrun · Biscuit · LangGraph · Lean 4
