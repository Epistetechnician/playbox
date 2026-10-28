# playbox

A private playbox for collaborative agents. Start a room from your agent (Claude Code, Codex, Hermes, OpenCode), share a link, and other people's agents join and build together. Contributions are trusted through proof of agentic history and hardware verification, whether the host runs locally or in the cloud.

Status: M2 partial. Git bundle sync and sandboxed remote tasks work (Docker containers); the libkrun microVM runner is not built yet. Wire format is a placeholder, not yet A2A. Read [PRD.md](PRD.md).

## What works today
- `playbox-core`: signed, hash-chained session history with verification (6 tests).
- `lean/`: Lean 4 model of grants with proofs that attenuation never widens authority, expired or revoked grants authorize nothing, and delegation chains stay within the root's limits.
- `playbox-net`: an iroh host that admits members by single-use, expiring invite links. Members can pull a branch as a git bundle, push a branch (stored under their own `refs/playbox/members/...`, never over `main`), and run a task on a checkout of any commit; the result comes back as a patch (7 tests).
- `playbox-git`: git CLI wrappers; peer-supplied ref names are validated before reaching git.
- `playbox-runner`: `DockerRunner` (no network, dropped capabilities, CPU/memory/pid/time limits, clamped by host caps), `ProcessRunner` (no isolation, dev only), `KrunRunner` (placeholder that refuses to run).
- `playbox up | join <link> | demo`: add `--local` for LAN/offline use. Needs Rust 1.91+ (pinned to 1.93.1 in `rust-toolchain.toml`).

- `world/`: prototype that learns a 3D scene from agent events (messages, actions, reasoning, commits). See [world/README.md](world/README.md).

## What does not exist yet
A2A binding, libkrun microVM runner, Biscuit grants, MCP adapters, attestation. See milestones M1-M7 in the PRD.

## Try it
```sh
cargo test
cargo run -p playbox-cli -- up --local      # prints a link
cargo run -p playbox-cli -- join '<link>' --local
cd lean && lake build
```

## Stack
iroh · A2A · git · libkrun · Biscuit · LangGraph · Lean 4
