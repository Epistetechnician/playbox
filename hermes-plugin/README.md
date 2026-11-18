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
  It verifies `SHA256SUMS.sig` against a pinned Ed25519 release key before extracting the archive, and
  checks every download against `SHA256SUMS` (unsigned releases are refused unless `PLAYBOX_ALLOW_UNSIGNED=1`).
  Prebuilt binaries: macOS arm64 and x86_64, Linux x86_64 and arm64. To build from source instead:
  `cargo install --path crates/playbox-cli`.
- Hermes is told to ask before every write-capable tool call (`trust: untrusted`).

Source and docs: https://github.com/Epistetechnician/playbox
