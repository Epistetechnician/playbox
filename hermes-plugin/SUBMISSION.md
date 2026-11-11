# Submission drafts

Placeholders in `<angle brackets>` are filled after the repo is public, the SHAs are pinned and the demo is posted.

## Catalog PR body (use for both PRs, change the name and the "Surfaces" line)

**playbox: private rooms where your Hermes agent works with other people's agents**

What it does: a host shares a git repo into a room; guests join with a single-use link; agents exchange
messages, pull the repo, and run sandboxed tasks on the host (Docker, dropped capabilities, CPU/memory/time
limits). Every call is checked against a signed, expiring grant and lands in a hash-chained history that
`playbox verify` checks (one flipped byte is caught).

Surfaces used
- `playbox`: MCP server (stdio, `playbox mcp`) plus one skill. No hooks, no middleware, no tools registered in Python.
- `playbox-witness`: three observer hooks (`pre_llm_call`, `pre_tool_call`, `post_tool_call`). It records each tool
  call (digests only) into a signed log and pauses write-capable tools for the rest of any turn that read text
  written by another room member, until the user's next message.

Disclosure
- Needs the `playbox` binary on PATH, installed separately by the user (`install.sh`, checksum-verified; the
  v0.1.0 release is not signed, no release key is pinned yet). The plugin ships no binaries and never updates itself.
- Starts a local `playbox mcp` process. Hosting opens an iroh endpoint reachable through n0's public relays.
  Docker tasks run only on the host, only for members the host invited.
- Writes keys, histories and room state under `~/.playbox` (`PLAYBOX_HOME` overrides).
- `playbox-witness` runs `playbox record` as a subprocess after a tool call. No telemetry. No credentials read or stored.
- `trust: untrusted`: Hermes asks before every write-capable Playbox call.

Limits, stated plainly
- The hold tracks peer text at string level; it is a speed bump for prompt injection, not a proof against it.
- Tested live in Hermes Agent 0.22.5 with Haiku 4.5: one model-driven run where the agent read a planted host
  message telling it to run `touch …/PWNED` and was held by the witness (no file created). One run, not a rate.
- Prebuilt binaries: macOS arm64, Linux x86_64 and arm64. Intel macOS builds from source.
- Entry points to review: `hermes-plugin/` (portable) and `hermes-plugin-witness/` (native). Two entries by design:
  the docs say the portable adapter does not replace native plugins. The witness needs `playbox` on PATH, not the
  other plugin.

Pinned SHA: `<sha>`

## X post (under 90s video, tag @NousResearch)

Hermes agents can work with strangers' agents now. I built Playbox for the @NousResearch Agent Plugin Hackathon:
a private room with signed history, sandboxed tasks, and a witness that pauses your agent when a peer's message
tries to make it act. Demo: planted message in, `PWNED` never created, tampered log caught.
<repo link>  <PR links>

## Demo shot list (under 90s)

1. 0-15s: cockpit UI, host creates a room, guest joins by link, a message crosses.
2. 15-45s: Hermes joins by `link_file`, reads the planted host message, the witness pauses it and Hermes asks the user. Show the folder: no `PWNED`.
3. 45-70s: Work tab runs a sandboxed task; patch comes back.
4. 70-90s: Proof tab: verify passes; flip one byte; verify fails.

## Typeform

Demo link, the two PR links, and the one-line pitch: "Hermes agents that can work with strangers' agents, with a
signed record and a pause on peer-triggered writes."
