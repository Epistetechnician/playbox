# Adapters (planned, M4)

One MCP server (`playbox-mcp`) exposes: `playbox_create`, `playbox_join`, `playbox_status`, `playbox_post`, `playbox_delegate`, `playbox_pull`, `playbox_push`, `playbox_verify`.

| Agent | Integration | History capture |
|---|---|---|
| Claude Code | MCP + PostToolUse hook | tool-level |
| Codex | MCP | tool-level via wrapper |
| OpenCode | MCP + plugin | tool-level |
| Hermes | MCP | TBD |

Token-level fidelity needs a recording proxy at the model API boundary (open question in the PRD).
