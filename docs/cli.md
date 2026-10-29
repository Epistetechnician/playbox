# `playbox` command line

`playbox --help` lists everything; `playbox <command> --help` gives each command's flags. State lives in
`$PLAYBOX_HOME` (default `~/.playbox`, or `--home DIR`). Run `playbox doctor` to see what this machine can do.

| Area | Commands |
|---|---|
| Rooms | `up` (host; `--console` for live `link / members / audit / revoke`), `join LINK [--task CMD]`, `shell [LINK]`, `bridge LINK`, `call TOOL k=v…`, `mcp` |
| Review | `review REPO [list\|show\|accept\|reject]`, `audit FILE` |
| Trust | `verify LOG`, `attest`, `pin list\|add\|remove`, `approver-key`, `approve`, `record` |
| Memory | `memory remember\|recall\|export\|ingest\|review` |
| Steering | `steer extract\|random\|manifest\|run\|probe`, `experiment e1\|e1-merge\|e6\|e6b\|audit-e6\|cache-audit`, `world demo\|test` |
| Other | `doctor`, `demo`, `completions bash\|zsh\|fish` |

`shell` runs every MCP tool by hand, as a host or a guest: `playbox shell LINK`, then `status`, `ping text=hi`,
`pull ref=main`, `delegate base=main command="cargo test"`, `steer …`, `push branch=x`. `help TOOL` shows its
arguments. Anything not given a typed command (`checkpoint`, `receipts_check`, `lineage_check`) runs with
`playbox call TOOL …`. The Python commands run in the repository's `steer/`, `orchestrator/` and `world/`
projects (their `.venv`, else `uv`); set `PLAYBOX_ROOT` if you run `playbox` from outside the checkout.

Revoking a member needs the live host: use `up --console` (or `revoke member=ID` in a `shell` that created the room).
`memory-review` still works as an alias for `memory review`.
