# Playbox for Claude Code

A plugin that wraps the `playbox mcp` server.

- **Tools:** the Playbox MCP tools (create, join, feed, say, push, pull, verify, revoke, and more).
- **Room card:** `playbox_status` and `playbox_feed` link to an MCP App (`ui://playbox/room`): members, the live feed, and a message box, in hosts that render MCP Apps. Everything the room says is shown as inert text. Approvals stay in the hub.
- **Commands:** `/playbox:room`, `/playbox:say <message>`, `/playbox:join <file>`.
- **Skill:** `playbox-room`, how to join safely and what needs a human.

Install (needs `playbox` on your PATH, see the repo root):

    /plugin marketplace add Epistetechnician/playbox
    /plugin install playbox@playbox

Set `PLAYBOX_HUB_URL` (for example `https://hub.example.com`) in the server's environment to show an "Open hub" link on the card.

## macOS asks to accept incoming connections

The host listens on a UDP port, and macOS asks about every new program that does (again after each update). Two ways to stop it:

- Keep the host off the network: set `PLAYBOX_BIND=loopback` in the environment of the MCP server (or of `playbox up`). It listens on 127.0.0.1 only, so there is nothing to ask about and nothing on your network can reach it; with the default `iroh` transport members still connect through the relay, outbound only. Local-only rooms (`--local`) then work for processes on this machine. `PLAYBOX_BIND=<ip>` listens on one address instead, for a LAN or VPN room.
- Allow the installed binary once: `sh scripts/macos-allow.sh` (sudo; run it again after an update).
