# Playbox Desktop

A thin native shell that runs the hub for you: one app that starts it, keeps it running, shows it in a window and a tray item, and stops it cleanly. The PWA is unchanged and is still how a phone or a second device reaches the hub. See `docs/desktop-plan.md` for the plan and the reasoning.

```
desktop/
  crates/pbd-core/   everything that decides behaviour, with no GUI dependency (supervision, claim, probes, directories, autostart, tray wording)
  src-tauri/         the Tauri 2 shell: window, tray, signals, quit
  shell-ui/          the one local page shown while the hub starts
```

## What it does today

- Starts the hub on `localhost:7717` using the hub's **existing home** (`~/.playbox-hub` or `PLAYBOX_HUB_HOME`): passkeys, rooms and invites carry over.
- **Attaches** to a hub that is already running (a LaunchAgent, a terminal) instead of fighting it for the port, and takes over only if it stops answering.
- Restarts a crashed or hung hub with growing delays; a hub that keeps failing opens a circuit breaker and waits for "Restart hub" in the tray.
- One app per user (an advisory file lock the OS releases on any exit, so there are no stale locks).
- Closing the window keeps it in the tray; Quit, `kill` and Ctrl-C stop the hub politely first, so its tunnel goes with it.
- Tray: status line, Open, Restart hub, Launch at login (a LaunchAgent / XDG autostart entry / per-user Run key; no administrator rights), Show logs, Quit.
- The window has **no IPC**: the hub is a remote origin and is granted nothing (`src-tauri/capabilities/main.json`).

## Run it from a checkout

```bash
cd desktop
PLAYBOX_DESKTOP_HUB_SCRIPT=../ui/playbox_hub.py cargo run -p playbox-desktop
```

| Variable | Meaning |
|---|---|
| `PLAYBOX_DESKTOP_HUB_SCRIPT` | run the hub from this path instead of the bundled copy |
| `PLAYBOX_DESKTOP_PORT` | hub port (default 7717) |
| `PLAYBOX_DESKTOP_HUB_ARGS` | extra hub flags, whitespace-separated (for example the Cloudflare tunnel options) |
| `PLAYBOX_HUB_HOME` | the hub's home |

## Tests

```bash
cd desktop
cargo test -p pbd-core          # supervision, probes, claim, layout, autostart, wording, and the real hub under the real supervisor
cargo clippy --workspace --all-targets -- -D warnings
```

The supervisor tests use real child processes: crash loops, a hung service, a service that never becomes healthy, attach and take-over, restart, and stopping during a long backoff.

## Passkeys

The embedded webview cannot do platform passkeys on `localhost` (measured; see `docs/desktop-plan.md`, Spike 1, and `spikes/passkey_probe.py` to repeat it). Sign in through the system browser for now; the window is for status and management.

## Bundling

`scripts/stage.py` assembles `desktop/stage/` (the hub, a relocatable Python, the `playbox` binary); `tauri.bundle.conf.json` packs it into the app's resources:

```bash
cargo build --release -p playbox-cli
python3 desktop/scripts/stage.py --playbox target/release/playbox \
  --python-archive cpython-….tar.gz --python-sha256 <hash>
cd desktop && cargo tauri build --config tauri.bundle.conf.json
```

The Python archive is passed with the hash it must have; the script refuses a mismatch or an archive that writes outside the stage. Without `--python-archive` the stage is for development and the app uses the system `python3`. When `bin/playbox` is present the app points the hub at it (`PLAYBOX_CLI`, `--mcp`).

The tray also has "Open in browser", because passkeys need a real browser (see above).

## Not built yet (see the plan)

Pinned Python runtime per platform and the CI job that builds and signs the bundle, notarization, updates, secrets in the OS keychain, and Windows (the `playbox` binary's control socket is Unix-only). `cargo tauri build` itself has not been run yet.
