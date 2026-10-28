# Adapters

One MCP server, `playbox-mcp`, works with any MCP-capable agent. Each agent just needs to launch it
over stdio. The snippets here follow each agent's documented MCP config format; **they are untested
against the real agents** (the server itself is tested end to end over stdio, see below).

## Install

```sh
cargo install --path crates/playbox-mcp   # the MCP server
cargo install --path crates/playbox-cli   # `playbox record` / `playbox verify`
```

## Configure your agent

| Agent | Where | File here |
|---|---|---|
| Claude Code | `.mcp.json` in your project, or `claude mcp add playbox -- playbox-mcp` | [claude-code/mcp.json](claude-code/mcp.json) |
| Codex | `~/.codex/config.toml` or `codex mcp add playbox -- playbox-mcp` | [codex/config.toml](codex/config.toml) |
| OpenCode | `opencode.json` in the project root, or `~/.config/opencode/opencode.json` | [opencode/opencode.json](opencode/opencode.json) |
| Hermes | `~/.hermes/config.yaml` | [hermes/config.yaml](hermes/config.yaml) |

Then just ask your agent: "Create a Playbox for ~/code/myrepo and give me the link" (host), or
"Join this Playbox: playbox://..." (guest).

## Tools

| Tool | Who | What |
|---|---|---|
| `playbox_create` | host | Start a room, share a git repo, optionally let members run sandboxed tasks (`docker: true`). Choose `transport` (`iroh`, `local`, `relay:<url>`) and a room `name` (a named room keeps its identity, members and bans across restarts). Returns a join link. |
| `playbox_join` | guest | Join with a link; get a scoped, expiring grant and a local workspace. Follows the transport recorded in the link unless you pass `transport`. |
| `playbox_status` | both | Members, workspace, history log path and public key. |
| `playbox_ping` | guest | Echo through the host; checks the connection and grant. |
| `playbox_pull` | guest | Fetch a branch into the workspace (optionally check it out). |
| `playbox_delegate` | guest | Run a shell command on the host in a sandbox against a commit; get output and a patch (optionally applied). |
| `playbox_push` | guest | Send a branch to the room, stored under your own namespace. |
| `playbox_verify` | both | Verify a history log's hash chain and signatures. |
| `playbox_revoke` | host | Revoke a member and bar their key. |

There is no free-form messaging tool yet.

## History capture (proof of agentic history)

- **Every agent:** `playbox-mcp` records each Playbox tool call and result into a signed, hash-chained
  log (`$PLAYBOX_HOME/history/mcp-<time>.log`, default `~/.playbox`). Only Playbox calls, not
  everything the agent does.
- **Claude Code, fuller:** add [claude-code/settings.hooks.json](claude-code/settings.hooks.json) to
  your Claude Code settings. The PostToolUse hook pipes every tool call to `playbox record`, which
  appends it to `$PLAYBOX_LOG` (default `~/.playbox/history/default.log`) signed with
  `~/.playbox/session.key`. Check any log with `playbox verify <log>`.
- Codex, OpenCode and Hermes have no equivalent hook wired up here yet.
- Token-level fidelity (the model's actual responses) needs a recording proxy at the model API
  boundary. Not built.

## Safety notes

- `docker: true` is the only way a member can run tasks. The sandbox has no network, dropped
  capabilities, and CPU/memory/time limits, but it is a container, not a microVM.
- Treat a join link like a password until it is used or expires.
- The session key is a plain file (mode 0600); there is no hardware binding yet (PRD tiers T1-T3).

## Choosing a transport

| You want | Say |
|---|---|
| Works anywhere, no setup | nothing (`iroh`, n0's public relays) |
| Same machine or LAN only, no internet | "use the local transport" |
| Your own infrastructure | "use relay https://relay.example.com" (run `iroh-relay` there; `PLAYBOX_RELAY_CA=/path/ca.pem` if it uses a private CA) |

Set `PLAYBOX_TRANSPORT` in the MCP server's environment to change the default.
