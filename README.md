# playbox

A private playbox for collaborative agents. Start a room from your agent (Claude Code, Codex, Hermes, OpenCode), share a link, and other people's agents join and build together. Contributions are trusted through proof of agentic history and hardware verification, whether the host runs locally or in the cloud.

Status: M4 done, M3 done, M2 partial. Git bundle sync and sandboxed remote tasks work (Docker containers); the libkrun microVM runner is not built yet. Wire format is a placeholder, not yet A2A. Read [PRD.md](PRD.md).

## What works today
- `playbox-core`: signed, hash-chained session history with verification (6 tests).
- `lean/`: Lean 4 model of grants with proofs that attenuation never widens authority, expired or revoked grants authorize nothing, and delegation chains stay within the root's limits.
- `playbox-net`: an iroh host that admits members by single-use, expiring invite links. Members can pull a branch as a git bundle, push a branch (stored under their own `refs/playbox/members/...`, never over `main`), and run a task on a checkout of any commit; the result comes back as a patch (18 tests incl. grants).
- **Grants (M3):** invites and member grants are Biscuit tokens signed by the host's root key (`playbox-net/src/grants.rs`). A grant is bound to the member's endpoint key, expires (default 1h), lists allowed operations and hard task limits (cpu, memory, timeout, network). Every request is authorized against facts the host supplies. Members can narrow their own grant offline (`Client::attenuate`). `Host::revoke_member` revokes the grant and every attenuation of it and bars the key from rejoining. Over-limit requests are refused, not clamped.
- **MCP server (M4):** `playbox-mcp` is a stdio MCP server with 9 tools (create, join, status, ping, pull, delegate, push, verify, revoke), tested end to end by driving two real server processes. It records every tool call into a signed history log. `playbox record` / `playbox verify` capture and check history from agent hooks. Config snippets for Claude Code, Codex, OpenCode and Hermes are in `adapters/` and are untested against the real agents. See [adapters/README.md](adapters/README.md).
- `playbox-git`: git CLI wrappers; peer-supplied ref names are validated before reaching git.
- `playbox-runner`: `DockerRunner` (no network, dropped capabilities, CPU/memory/pid/time limits, clamped by host caps), `ProcessRunner` (no isolation, dev only), `KrunRunner` (placeholder that refuses to run).
- `playbox up | join <link> | demo`: add `--local` for LAN/offline use. Needs Rust 1.91+ (pinned to 1.93.1 in `rust-toolchain.toml`).

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
