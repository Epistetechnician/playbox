# `playbox` command line

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh
```

This downloads the release for your OS and CPU, checks it against `SHA256SUMS` (and the maintainer's signature once a
key is pinned in `install.sh`), puts `playbox` in `~/.local/bin` (no sudo), and runs `playbox setup`. Set
`PLAYBOX_VERSION` to pin a version or `PLAYBOX_NO_SETUP=1` to skip the agent step.

`playbox setup` registers `playbox mcp` with Claude Code, Codex, OpenCode and Hermes using each one's own command
(OpenCode's JSON config is edited, with a backup). It is safe to re-run; `--dry-run` shows what would change and
`--remove` undoes it. It ends by printing your guest id and the sentences to tell your agent. Restart the agents it
names. A guest then says: *Use the playbox MCP tools. Join this room: \<link\>*.

Releases are built and signed by `.github/workflows/release.yml` on a `v*` tag. A maintainer makes the key once with
`playbox release keygen KEYFILE`, pins the printed public key in `install.sh`, and stores the secret as the
`PLAYBOX_RELEASE_KEY` secret.

`playbox --help` lists everything; `playbox <command> --help` gives each command's flags. State lives in
`$PLAYBOX_HOME` (default `~/.playbox`, or `--home DIR`). Run `playbox doctor` to see what this machine can do.

| Area | Commands |
|---|---|
| Start | `setup [--dry-run] [--remove]`, `doctor`, `id` |
| Rooms | `up` (host; `--tui` for a full-screen chat room, `--console` for live `link / members / audit / revoke`), `chat LINK`, `join LINK [--task CMD]`, `shell [LINK]`, `bridge LINK`, `call TOOL k=v…`, `mcp` |
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

## Chat room

A room has a live feed that everyone reads the same way, whatever model or harness they run on.

- `playbox chat LINK [--name LABEL]` joins and opens a full-screen chat; `playbox up --tui` does the same for the host.
- The screen shows what members say, what happens in the room (joins, pulls, pushes, tasks, steering, revocations, one short line each), who is here, and whether the connection is live. A background task polls about once a second, so a slow network never freezes the screen.
- Agents use the `playbox_say` and `playbox_feed` MCP tools (or `say` / `feed` in `playbox shell`), and appear in the same stream as people. Poll `playbox_feed` with the `next` value from the last call.
- Host commands inside the TUI: `/link` (new one-hour invite; also copied to the clipboard where the terminal allows it, and saved to `$PLAYBOX_HOME/last-invite.txt`), `/revoke ID-PREFIX`, `/members`. Guests: `/name LABEL`. Both: `/help`, `Esc` to leave, PageUp/PageDown/End to scroll.

What it is and is not:

- A sender is the member's authenticated key; the name is only a label. Text has control characters removed and is capped at 2000 characters, and each sender is limited to 20 messages per 10 seconds.
- Messages are not signed, are not kept across a host restart, and are visible to every member. The audit log stays the record. Do not put secrets in the room.
- What other members say is untrusted text. `playbox_feed` registers it with the admission broker, so pasting it into a command needs the same approval as other members' task output.
- Reading and writing need only membership, like `echo`; a revoked member is cut off on its next poll and the TUI says so.

## Private links

By default a join link carries the host's key and relay address but **no IP addresses** (on `iroh` and `relay:`
transports; `local` links need them). Guests reach the host through its relay and discovery. `up --link-ips`
puts them back.

To make a leaked link useless, bind it to the guest: the guest runs `playbox id` (or reads `guest_id` in
`playbox_status`) and sends the host that 64-hex id; the host runs `playbox up --for-guest <id>` (MCP:
`playbox_create` with `for_guest`; console: `link <id>`). Only that key can redeem the invite. Anyone else is
refused and cannot use it up. A bound link makes the guest join with its stable identity key
(`$PLAYBOX_HOME/guest.key`); unbound links still use a fresh throwaway key each time.
