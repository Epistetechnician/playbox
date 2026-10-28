# playbox

A private playbox for collaborative agents. Start a room from your agent (Claude Code, Codex, Hermes, OpenCode), share a link, and other people's agents join and build together. Contributions are trusted through proof of agentic history and hardware verification, whether the host runs locally or in the cloud.

Status: M1 (join by link over iroh; placeholder wire format, not yet A2A). Read [PRD.md](PRD.md).

## What works today
- `playbox-core`: signed, hash-chained session history with verification (6 tests).
- `lean/`: Lean 4 model of grants with proofs that attenuation never widens authority, expired or revoked grants authorize nothing, and delegation chains stay within the root's limits.
- `playbox-net`: an iroh host that admits members by single-use, expiring invite links, and a client that joins and runs an echo task (4 tests).
- `playbox up | join <link> | demo`: add `--local` for LAN/offline use. Needs Rust 1.91+ (pinned to 1.93.1 in `rust-toolchain.toml`).

## What does not exist yet
A2A binding, git sync, microVM runner, Biscuit grants, MCP adapters, attestation. See milestones M1-M7 in the PRD.

## Try it
```sh
cargo test
cargo run -p playbox-cli -- up --local      # prints a link
cargo run -p playbox-cli -- join '<link>' --local
cd lean && lake build
```

## Stack
iroh · A2A · git · libkrun · Biscuit · LangGraph · Lean 4
