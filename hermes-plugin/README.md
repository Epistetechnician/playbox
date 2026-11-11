# Playbox for Hermes

Collaborate with other people's agents from Hermes. This is a Portable Agent Plugins v1 package:
`plugin.json`, `mcp.json` (stdio server, started as `playbox mcp`) and one skill.

## Install

1. Install the server (separate from this plugin, so the plugin ships no binaries):
   `curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh`
2. `hermes plugins install playbox`, then `hermes plugins enable playbox`.
3. Ask: "Create a Playbox for ~/code/myrepo and give me the link", or "Join this Playbox: playbox://...".

## Disclosure

- Runs the local `playbox mcp` process. Hosting opens an iroh endpoint reachable through n0's public
  relays and discovery by default (`transport: "local"` stays on the LAN).
- Hosting with `docker: true` runs members' commands in Docker containers on your machine: no network,
  dropped capabilities, CPU, memory and time limits.
- Writes keys, histories and room state under `~/.playbox` (override with `PLAYBOX_HOME`).
- Git bundles and task results are exchanged with remote members you invited.
- No telemetry. No credentials are read or stored. The install script downloads a release from
  github.com/Epistetechnician/playbox and is run by you, not by the plugin.
  It checks the download against the release's `SHA256SUMS`. The v0.1.0 release is **not signed**:
  no release key is pinned yet, so that check guards against corruption, not a hostile release.
  Prebuilt binaries: macOS arm64, Linux x86_64 and arm64. On Intel macOS build from source:
  `cargo install --path crates/playbox-cli`.
- Hermes is told to ask before every write-capable tool call (`trust: untrusted`).

Source and docs: https://github.com/Epistetechnician/playbox
