# Adapters

One MCP server, `playbox-mcp`, works with any MCP-capable agent. Each agent just needs to launch it
over stdio. The snippets here follow each agent's documented MCP config format; **tested on 2026-10-29
with the real agents' MCP loaders** (below) (the server itself is tested end to end over stdio, see below).

## Install

```sh
cargo install --path crates/playbox-mcp   # the MCP server
cargo install --path crates/playbox-cli   # `playbox record` / `playbox verify`
```

The release archive from `install.sh` holds only `playbox`, not `playbox-mcp`. Use `playbox mcp` (same server) as the command in the snippets
below, or let `playbox setup` write it for you.

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

The server lists 41 tools (`tool_list()` in `crates/playbox-mcp/src/lib.rs`; `playbox shell` then `help` prints them). The table is the
core room set; the others are:

- **Talk:** `playbox_say`, `playbox_feed`, `playbox_leave`, `playbox_live_ready`, `playbox_conversations`, `playbox_conversation_new`,
  `playbox_conversation_invite`, `playbox_conversation_say`, `playbox_conversation_read`.
- **Host services:** `playbox_capabilities`, `playbox_capability_call`, `playbox_browse`, `playbox_room_remember`, `playbox_room_recall`,
  `playbox_steer`, `playbox_audit`.
- **Your own memory and proofs:** `playbox_remember`, `playbox_recall`, `playbox_memory_ingest`, `playbox_memory_export`,
  `playbox_receipts_check`, `playbox_checkpoint`, `playbox_checkpoint_verify`, `playbox_holdout_commit` / `_prove` / `_verify`,
  `playbox_evaluator_issue`, `playbox_variant_sign`, `playbox_evidence_sign` / `_check`, `playbox_lineage_check`.

| Tool | Who | What |
|---|---|---|
| `playbox_create` | host | Start a room, share a git repo, optionally let members run sandboxed tasks (`docker: true`). Choose `transport` (`iroh`, `local`, `relay:<url>`) and a room `name` (a named room keeps its identity, members and bans across restarts). Returns a join link, which carries no IP addresses by default; pass `for_guest` (the guest's id from `playbox id`) to make it work only for them. |
| `playbox_join` | guest | Join with a link; get a scoped, expiring grant and a local workspace. Follows the transport recorded in the link unless you pass `transport`. |
| `playbox_status` | both | Members, workspace, history log path and public key. |
| `playbox_ping` | guest | Echo through the host; checks the connection and grant. |
| `playbox_pull` | guest | Fetch a branch into the workspace (optionally check it out). |
| `playbox_delegate` | guest | Run a shell command on the host in a sandbox against a commit; get output and a patch (optionally applied). |
| `playbox_push` | guest | Send a branch to the room, stored under your own namespace. |
| `playbox_verify` | both | Verify a history log's hash chain and signatures. |
| `playbox_use_host` | guest | Send your calls to a replica of the room you joined (or back to `primary`), using the grant you already have. Vetted like a join; the replica must accept your grant first. |
| `playbox_revoke` | host | Revoke a member and bar their key. |

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

- A member can run tasks only if the host passes `runtime` (`docker: true` is the shorthand for `runtime: "docker"`; `libkrun` runs each task in a
  microVM, `openshell` is the third option). The Docker sandbox has no network, dropped capabilities, and CPU/memory/time limits, but it is a
  container, not a microVM.
- Treat a join link like a password until it is used or expires.
- The session key is a plain file (mode 0600); there is no hardware binding yet (PRD tiers T1-T3).

## Choosing a transport

| You want | Say |
|---|---|
| Works anywhere, no setup | nothing (`iroh`, n0's public relays) |
| Same machine or LAN only, no internet | "use the local transport" |
| Your own infrastructure | "use relay https://relay.example.com" (run `iroh-relay` there; `PLAYBOX_RELAY_CA=/path/ca.pem` if it uses a private CA) |

Set `PLAYBOX_TRANSPORT` in the MCP server's environment to change the default.

## Tested

| Agent | Version | Result |
|---|---|---|
| Codex | 0.160.1 | `playbox_status` called by the model through `codex exec`; the server's history log shows the call. |
| OpenCode | 1.19.4 | `playbox_status` called by a model (`opencode-go/glm-5.3-flash`) through `opencode run`; the server's history log shows the call. |
| Hermes | 0.22.5 | `playbox_status` called by a model (`opencode-go/glm-5.3-flash`) via `hermes -z`; the server's history log shows the call. Hermes loads MCP tools lazily, so the prompt has to name the `playbox` server. |
| Claude Code | (version not recorded) | A headless Claude Code joined a room and ran a task as a second member. |
