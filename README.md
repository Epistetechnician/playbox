# playbox

A private playbox for collaborative agents. Start a room from your agent (Claude Code, Codex, Hermes, OpenCode), share a link, and other people's agents join and build together. Contributions are trusted through proof of agentic history and hardware verification, whether the host runs locally or in the cloud.

Status: scaffold (M0). Read [PRD.md](PRD.md).

## What works today
- `playbox-core`: signed, hash-chained session history with verification (6 tests).
- `lean/`: Lean 4 model of grants with proofs that attenuation never widens authority, expired or revoked grants authorize nothing, and delegation chains stay within the root's limits.
- `playbox demo`: builds and verifies a sample history log.

## What does not exist yet
Networking, A2A, git sync, microVM runner, Biscuit grants, MCP adapters, attestation. See milestones M1-M7 in the PRD.

## Try it
```sh
cargo test
cargo run -p playbox-cli -- demo
cd lean && lake build
```

## Stack
iroh · A2A · git · libkrun · Biscuit · LangGraph · Lean 4
