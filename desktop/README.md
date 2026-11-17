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

## Signing in

The window signs itself in to the local hub without a passkey: the app writes a one-use, owner-only proof file into the hub's private home (`desktop-login/`, same root of trust as `playbox_hub.py --pair`), then opens `/desktop/signin`. The hub checks and deletes the file and opens a *weak, local-only* session: it is ignored on any request that is not loopback (a stolen cookie is nothing through the tunnel), and sensitive actions still need a real passkey from a browser. It needs a hub that has this flow (`/api/auth/state` reports `local_signin`); with an older hub the window just shows the lock screen. Tray: "Sign in again".

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

The Python runtime is pinned in `python-runtime.json` (hashes from the release's `SHA256SUMS`; `scripts/fetch_python.py <target> <dir>` downloads and verifies it). `.github/workflows/desktop-bundle.yml` builds `Playbox.app`, ad-hoc signs it, smoke-tests that it starts its own hub with the bundled Python, and uploads a zip with its SHA-256. Built for Apple silicon and Intel (Intel is cross-compiled; CI runs it only where Rosetta exists). Each is about 125 MB.

`desktop.log` in the app's log folder records every supervisor state change with its reason ("could not start …"), so a hub that did not start always has an explanation on disk.

## Updates

The tray's "Check for updates" item (and a daily background look, packaged app only) fetches the latest release's `SHA256SUMS` and `.sig`, verifies them against the pinned `release.pub` key, checks `release.json` and `Playbox-macos-arm64.zip` against the signed list, and offers the update. Installing is always a click unless the host sets `PLAYBOX_DESKTOP_UPDATE=install`; `off` disables looking. The new app is unpacked next to the old one, must pass `codesign --verify` and report the promised version via `--version`, then replaces the old one (kept as `Playbox.previous.app`) and relaunches. Any failure leaves the installed app untouched. Same trust as `playbox update` (docs/releasing.md); stable releases only; macOS Apple silicon and Intel (`Playbox-macos-arm64.zip`, `Playbox-macos-x86_64.zip`; each app updates from its own). `scripts/set_version.py` makes the built app report the release's version.

## Not built yet (see the plan)

Linux and Windows bundles, canary channel, rollback from the menu, notarization (deferred: unsigned by decision), updates, secrets in the OS keychain, and Windows (the `playbox` binary's control socket is Unix-only). 
