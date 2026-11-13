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
| Start | `init`, `setup [--dry-run] [--remove]`, `doctor`, `id`, `hub` |
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

## Setting up a machine

`playbox init` sets Playbox up end to end, in six steps: it checks the machine (including a different `playbox` earlier on your PATH), creates your keys with
private permissions, registers Playbox with the coding agents it finds, finds the hub and asks how to reach it (this computer, a Cloudflare quick tunnel, or
your own tunnel), runs the hub as a background service that starts at login and restarts if it stops (launchd on macOS, a systemd user unit on Linux, no
root), and waits until the hub answers before printing your first pairing link. Each step looks at the current state first, so it is safe to re-run and only
changes what is missing. `--dry-run` shows the changes without making them; `--yes` asks nothing (flags, then what was remembered, then this computer only);
`--no-service`, `--no-agents`, `--local`, `--cloudflare`, `--tunnel NAME --hostname HOST`, `--port`, `--lan` and `--hub-dir` pre-answer the questions.
It never replaces a hub service it didn't write without a yes, never takes over one loaded from another file, and won't register agents from a build-tree binary.
Manage the service later with `playbox hub --service install|status|uninstall` (the log is `$PLAYBOX_HOME/hub.log`).

## The hub

`playbox hub` starts the dashboard app (passkey sign-in, every room in one place, invite pages) on this machine and opens it in your browser.
The first time, on a terminal, it asks how you want to reach it and remembers the answer in `$PLAYBOX_HOME/hub.json`:

1. Only on this computer.
2. Cloudflare quick tunnel: HTTPS from anywhere, free, no account, a new address each run. Needs `cloudflared`.
3. Your own Cloudflare tunnel: a stable address, so passkeys survive restarts. Give it a tunnel name (or id) and a hostname; if the tunnel doesn't exist
   it offers to create it and route the hostname (`cloudflared tunnel login` once first).

Flags skip the question for one run: `--local`, `--cloudflare`, `--tunnel NAME --hostname HOST`. `--reconfigure` asks again. Also `--port`, `--lan`, `--pair`, `--no-open`;
anything after `--` goes to the hub (`playbox hub -- --help`). With no terminal and nothing remembered it runs locally. The hub is found in the installer's
directory, a checkout, `--hub-dir` or `PLAYBOX_HUB_DIR`; install it with `install.sh --hub --no-start`. See [ui/README.md](../ui/README.md) for the hub itself.

## Private links

By default a join link carries the host's key and relay address but **no IP addresses** (on `iroh` and `relay:`
transports; `local` links need them). Guests reach the host through its relay and discovery. `up --link-ips`
puts them back.

To make a leaked link useless, bind it to the guest: the guest runs `playbox id` (or reads `guest_id` in
`playbox_status`) and sends the host that 64-hex id; the host runs `playbox up --for-guest <id>` (MCP:
`playbox_create` with `for_guest`; console: `link <id>`). Only that key can redeem the invite. Anyone else is
refused and cannot use it up. A bound link makes the guest join with its stable identity key
(`$PLAYBOX_HOME/guest.key`); unbound links still use a fresh throwaway key each time.
