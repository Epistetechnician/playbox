# Changelog

All notable changes to Playbox. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Edit **Unreleased** as you work; `playbox release prepare X.Y.Z` turns it into the
release section (see `docs/releasing.md`). `playbox update --check` shows the newest section to anyone running an older build.

Wire protocol: every release states its protocol number in `release.json`. A protocol bump means older builds cannot join rooms
hosted by the new one (and the reverse); a release without one stays compatible with the previous release.

## [Unreleased]

### Added
- A running host's control socket answers `members` and `revoke MEMBER ID` (the same action as the console's `revoke`; the socket is private to the user), so a host started as a service can have a member removed. In the hub, Revoke on an adopted room goes through it (passkey step-up).
- `playbox up --grant-ttl SECS`: how long each member's grant lasts (default one hour, at most 30 days). The MCP `playbox_create` tool already had this;
  the command-line host did not, so agents that ran past an hour were refused ("not authorized") with no way to lengthen the grant.

### Fixed
- A host whose state directory is called `state` is named after the folder around it (`~/.playbox-rooms/gpu-lab/state` is `gpu-lab`), not `state`, in the hub's list of hosted rooms.

## [0.3.6] - 2026-11-16

### Added
- The host records how each member was admitted (when the grant was issued and runs out, which link, the limits and the hosts, secrets and capabilities it allows; never a token) and `playbox_status` shows it. The Harnesses tab gets a Grant button per member with the delegation (host to member), expiry and limits, and a Revoke button (passkey step-up). A new member joining a room where you endorse agents raises a quiet attention item.

## [0.3.5] - 2026-11-16

### Added
- Owner endorsement (Harnesses tab): vouch for one agent key in one room with a passkey touch. The endorsement is bound to the card's content, so a routine re-announce keeps it and any change to the name, machine, role, harness, model or public name breaks it: the agent shows "endorsed, card changed" and an urgent item asks you to look. Remove it the same way.
- Research view is data-driven: `scripts/gen_research.py` generates `ui/research/*.json` from `docs/benchmark.md` and the F3 pilot receipts (CI runs it with `--check`), and each run has a Samples tab (filterable table, then a collapsible step trail per sample).
- Anchored comments: `::pbx comment {"on":"note:<id>|artifact:<id>|event:<seq>","text":...}` on the board model, a comment thread per note, a Comment button per timeline event, a Comments count in the room header, and a `/playbox:comment` command.
- Canvas: a Harnesses panel (agents grouped by the harness and model they report, with machines and who is online), Research and Cockpit links in the top bar, phone reading view for drilled panels (they reflow to one narrow column instead of shrinking), higher-contrast small text in light mode and every theme, and larger tap targets on touch.
- Settings > Notifications (Push, Alerts, Rooms): subscribe, unsubscribe and test push from Settings; the "unreachable for how long before it is urgent" threshold is now a hub setting (`/api/settings/notify`, passkey step-up to change; `PLAYBOX_DOWN_ALERT_S` stays the default); a room can be muted so it never sends a push while its items still show in Attention.
- Approvals in the hub: a host's control socket now answers `calls`, `approve ID` and `deny ID`; the hub lists calls waiting for the owner in Attention (Approve / Deny) and in Settings > Safety > Approvals, with a passkey step-up and an audit entry per decision. Hosts on an older build simply list nothing.
- Signed agent cards (`playbox_announce`): an agent introduces itself with a card signed by its own key, bound to the room and valid for at most 24 hours. The hub verifies it against the key the room says sent the message, shows `signed`, `card expired` or `unidentified` on the Harnesses tab, reports a refused card once, and records changes (a new harness or model under the same key) and names used by two keys.
- A full Settings screen (⌘, or Ctrl+,; the gear in the top strip; the Hub tab; the status bar; the command palette): a searchable tree with a breadcrumb. Appearance (theme, accent, typography, layout, custom CSS, import and export, reset), Interface (rooms sidebar, status bar, landing view, 24-hour clock, reduced motion), Hub (devices, sessions, network), Safety (posture), keyboard shortcuts and About. Appearance and Interface settings are kept in the browser; device and session changes go through the hub with the usual passkey step-up.
- Notes and Work results render maths (KaTeX), coloured code (Shiki: python, rust, js, ts, shell, json, diff, lean, markdown, yaml, toml) and tables, with links sanitised by DOMPurify. The three libraries are vendored in `ui/static/vendor/` (licences in `LICENSES.md`), served by the hub itself, and load only when a note needs them. A strict markdown module (`markdown.js`) also renders text that is still arriving: an open code fence or `$$` block stays as plain text. Images from other sites are never fetched.
- A Research view in the hub: experiments as runs (list, then Overview, Results, Usage, Timeline, Config and JSON tabs, in the style of a log viewer). It ships the room-vs-silos benchmark and the F3 H100 pilot; add a run by dropping a JSON file in `ui/research/`.

### Changed
- A plain `::pbx agent` message no longer makes an identity: it is shown only as an unsigned claim beside an unidentified member. Agents should call `playbox_announce`.

## [0.3.4] - 2026-11-15

### Changed
- The default look is now Refined: flat surfaces, 6px corners, no page glow or shadows, plain numbers and a plain wordmark. The earlier look (glows, shadows, dot-matrix digits and wordmark) stays as Style: Expressive, and Density: Compact shows more per screen. Both are in Appearance settings.
- New look on every page, from one stylesheet (`ui/static/theme.css`): light olive green is the primary colour, a blue gradient marks the selected tab and your own data, amber is for warnings, neutral olive-grey surfaces, sans-serif controls. Palette and the blue selected-tab treatment follow mlsweep (Apache-2.0).
- The cockpit and the hub home page have instrument panels: a Stats card (key/value rows, message-rate sparkline, who-spoke bar, event breakdown), an Activity card (swim-lanes on one time axis: per member and per kind in a room, per room on the hub) and a collapsible Trail. Cards are rounder and softer to match.

### Added
- A Harnesses tab in each room: members grouped by the tool they run in (Hermes, Claude Code, Codex, anything), with model, machine, role, presence and message count, plus the harnesses this hub can connect on this machine. Agents announce themselves with the new optional `harness` and `model` fields of `::pbx agent` (text from the agent, clipped, shown as plain text).
- A Notes tab in each room: the room's artifacts (proofs, code, documents, datasets, charts, notes) as a file tree grouped by kind, with a document view (Live or Source), status and check chips, and revision history. Text from members is escaped; no links, HTML or images are rendered.
- Appearance settings (Hub tab, command palette, status bar): ten themes with dark and light sides (Playbox, Mono, Catppuccin, Nord, Solarized, Everforest, Tokyo Night, GitHub, Gruvbox, Dracula), accent colour, fonts, interface scale, your own CSS, theme import and export. All kept in the browser; the model follows Hermes Agent's desktop themes (MIT).
- A rooms sidebar (grouped by machine, with state and age) and a status bar (hub health, public address, theme) on the home page and in every room; a room header and a Timeline tab (event bars, kind filters, searchable history table) in the cockpit.

## [0.3.3] - 2026-11-15

### Added
- The System view has a Health card (tunnel, each room's host, last error, grant renewal), and `/api/system` carries a `health` section.
- `ui/service/install-host-macos.sh` runs a room host as a LaunchAgent that restarts and survives logout.
- The hub service runs a pinned copy of the hub (`~/.playbox/hub-app/current`), so switching branches in a checkout no longer changes what is running; `install-macos.sh --repin` updates it, `--live` keeps the old behaviour.
- The hub backs up its state once a day (last seven kept) and pushes an alert when the public address or a room's host stays down for five minutes (`PLAYBOX_DOWN_ALERT_S`). A joined room whose grant ran out says it needs a new invite.
- `scripts/demo.sh --live` (`scripts/demo_live.py`): the hill-climb through a live room. Guests pull the frontier, run a proposal as a sandboxed task, and push; the host's gate (`challenge/f3/room_gate.py`) scores each push with the F3 harness against the sealed holdout, signs a receipt, and moves `main` only on a promotion. A push that is not built on `main` or touches any file but `candidate.py` is refused without a receipt. The holdout is then revealed and the receipts verified. See `docs/demo.md`.
- `scripts/demo.sh` and `docs/demo.md`: one story, each step checked by the script: a guest joins by link and runs a sandboxed task, a spent invite is refused, the audit log is verified and an edited copy rejected, and a recorded hill-climb is re-verified from its receipts with a forged promotion rejected. Run on macOS and from a fresh clone on a new Ubuntu 24.04 machine.
- `docs/security-roadmap.md`: what is built, what is next and what is not planned, measured against Anthropic's guidance for working with advanced models.

### Fixed
- `playbox up` removes its hosting record and control socket on SIGTERM too, and the hub removes records left by hosts that were killed.
- `playbox doctor` called a research venv ready if a `.venv` folder existed. It now needs a working `python` and an existing base interpreter, so a folder copied from another machine is reported.
- `challenge/f3/verify.py` on a climbmix run without its data files ended in a traceback; it now fails the data-manifest check with what is missing.
- `challenge/f3/verify.py` compared a run's pinned protocol hash with `PROTOCOL.md` as it is today, so appending amendment A3 made the earlier dry run stop verifying. It now accepts a pinned version that was committed before the run began and names the commit (amendment A4).
- `cargo test --workspace` failed on Ubuntu 24.04, where AppArmor forbids unprivileged user namespaces: the rootless jail cases now skip with the reason, as they do when the kernel refuses `unshare`, and the networked case no longer waits forever when it skips.
- A libkrun task with a network could not resolve names: the guest's `/etc/resolv.conf` was empty. It now gets `/etc/hosts` with its pinned addresses (host list) or a resolver it may reach, 1.1.1.1 and 9.9.9.9 when the host has none; `PLAYBOX_GUEST_DNS` overrides (BOX-8, decision 44).
- `up --runtime libkrun --confine` as root failed on a machine that had not run Playbox since it booted (`/sys/fs/cgroup/playbox is not a writable cgroup v2 directory`); the host now creates that directory.
- Hub: a room that should be hosting but is not (the first attempt was refused, for instance because the relay was unreachable at startup) is hosted again on a later tick, and the reason is shown instead of dropped.
- `playbox --version` named the commit of the last branch switch; it now follows new commits.

### Security
- A libkrun task that asks for a network is refused on a host that cannot filter it (macOS, and Linux without `--confine`). libkrun's TSI gives the guest the host's loopback, LAN and cloud metadata service, so before this such a task could reach them. The host owner can accept that with `PLAYBOX_ALLOW_UNFILTERED_NETWORK=1` (`1`, `on` or `true`); a member or an agent cannot. Tasks without a network are unaffected (BOX-5, decision 42).

### Changed
- **Behaviour change:** the safety overrides `PLAYBOX_ALLOW_UNSAFE_RUNNER`, `PLAYBOX_ALLOW_MISSING_LIMITS` and the new `PLAYBOX_ALLOW_UNFILTERED_NETWORK` are now on only when set to `1`, `on` or `true`. Before, the first two were on for any value, including `0` and an empty string. A host that set one to anything else must set it to `1`.

### Testing
- CI: a `krun` job runs the libkrun boot tests, the escape drill and the confined-guest tests on a KVM runner, and fails on any skip (`PLAYBOX_CONFINE_REQUIRE_ALL`); a `lint` job runs `cargo fmt --check` and clippy with warnings as errors; an `f3` job runs the F3 tests and re-derives the recorded run.
- The Linux confinement around a real libkrun guest (`tests/confine_guest.rs`, root, KVM): boot and run under the jail, cgroup, syscall filter and network filter; no network unless asked; with a network, a public address is reachable and the host's loopback and the metadata address are not, with an unfiltered control that does reach loopback. Passed by hand on nested arm64 KVM (BOX-3).
- The escape drill (`cargo test -p playbox-runner --test escape`): breakout probes run inside a real libkrun guest and all must fail, with a canary that proves the probes ran. It skips where libkrun, its helper or Docker is missing (BOX-6, decision 43).

## [0.3.2] - 2026-11-14

### Added
- Hub: **a durable copy of every room's feed**, kept on this machine (`<room home>/archive/`, private, size-capped). When a room's host goes away the canvas, cockpit and room list show what the hub saved, marked offline, with **Download history** (JSON lines). Removing a room deletes its archive.
- Hub: a warning bar when the Cloudflare Access sign-in expires, with a Sign in again button.
- `scripts/dev-sign.sh`: sign playbox binaries with one stable local identity so the macOS firewall's answer survives rebuilds; `playbox update` re-signs with it when it is set up.
- Hub: **Adopt** a room hosted on this machine. A `playbox up --state DIR` host now publishes itself (a private socket under `~/.playbox/hosting`), and Add room > Join lists it with one button; adopting asks for a passkey touch.

### Security
- A capability call's authority is checked again when the owner approves it and while it runs. A member revoked, banned or removed in between gets no run, and a run in progress is stopped (call state `revoked`; audit `capability_refused` / `capability_cancelled`).
- Capability arguments with bidi, zero-width or separator characters are refused, and the host console shows the whole member id and escapes what a member typed.

### Fixed
- Hub: reads of a room have deadlines, so an unreachable host can no longer stall the poller (and every other room's updates).
- Hub: an adopted room joins its host again by itself when its hour-long grant ends or the hub restarts (before, its feed and canvas went blank).
- Host: a member that is refused again and again for the same reason is recorded once a minute, and a flood of events can no longer push chat messages out of the feed.
- Releases now include an Intel Mac (`darwin-x86_64`) archive, cross-compiled; the hub bundle (`playbox-hub.tar.gz`) was already built, so `install.sh --join` finds both.
- The hub and the cockpit no longer wait on a reverse DNS lookup before they listen (it stalled startup on some macOS machines).

### Testing
- `docs/requirements.md` gives each promise an ID; `conformance/trace.py` (a CI step) fails when a shipped promise has no check that names it.
- `installed_upgrade`: an older release installed with `install.sh` hosts a room, is upgraded, and the room, the member and the audit chain carry over.

### Fixed
- Hub: it no longer waits on a reverse-DNS lookup (`socket.getfqdn`) before it starts answering; on a machine with a slow resolver, startup could take many seconds.
- Hub: the Cloudflare tunnel is retried in the background (with backoff) if it fails to start or later drops, instead of the hub staying offline until restarted.

### Changed
- Guests dial out only: no UDP socket on the iroh and relay transports (the connection goes through the relay; `PLAYBOX_GUEST_DIRECT=1` keeps direct paths), and a socket on 127.0.0.1 only for a host on the same machine. macOS no longer asks to accept incoming connections for a program that only joins rooms.

## [0.3.1] - 2026-11-13

### Added
- Room canvas in the hub: a zoomable, drillable view of a room (members, feed, board, artifacts, metrics).
- Hub System view: the secure workflow, live. `playbox secure` checks the machine, agents and hub.
- `playbox_feed` can wait (`wait_secs`) so agents answer the moment someone speaks; the Playbox MCP app room card and Claude Code plugin.

### Fixed
- Hub: a refused POST no longer corrupts the next request on the same connection (step-up failed with 501 through the tunnel).
- Hub: friends can open invite links through Cloudflare Access; rooms are hosted again after a restart.

## [0.3.0] - 2026-11-13

### Security
- Releases are signed. `SHA256SUMS` (which lists every archive, `release.json` and the SBOM) carries an ed25519 signature; `install.sh` and `playbox update` refuse a release without a valid one. The trusted keys are `release.pub` in the repository, built into the binary. Releases before 0.3.0 are unsigned: `PLAYBOX_ALLOW_UNSIGNED=1` installs one on purpose.
- Releases carry a CycloneDX SBOM (`playbox.cdx.json`) and a Sigstore build-provenance attestation (`gh attestation verify FILE -R Epistetechnician/playbox`).
- Capability results are signed by the host. A caller checks the signature against the host id it dialed and the result hash against the output, so a relayed or copied result cannot be altered or passed off as another member's or another host's. A host before 0.3.0 does not sign, and its capability results are refused.
- CI: `cargo deny` (licences, banned crates, registry sources, advisories) is required; fuzzing runs on every push to `main` and weekly, with new targets for capability definitions and arguments and for the calls and conversations state files.

### Testing
- A version-skew scenario runs the current build and the previous release in one room, in both directions.
- A restart scenario kills the host and starts it again: finished capability calls are not run twice, a call that was running is abandoned, talk and the audit chain survive.

## [0.2.1] - 2026-11-13

### Fixed
- The release workflow publishes `playbox-hub.tar.gz`; 0.2.0 shipped without it, so `install.sh --hub` and `--join` could not install the hub from that release.

### Added
- Room tools: `playbox_watch` (proactive room watcher over MCP) and `browse` (members read web pages through the host's browser, limited to an allowlist).
- Hub: Integrations tab (agent harnesses and Composio apps), one-step hosting, friend invites through Cloudflare Access; Claude Code plugin and MCP App room card.

## [0.2.0] - 2026-11-13

### Added
- `playbox update`: check for, verify and install a newer release; `--check` (exit status 10 when one exists), `--version`, `--rollback`, `--json`.
  Archives are checked against `SHA256SUMS`, which is checked against the release key built into the binary; the old binary is kept for `--rollback`.
- Releases publish `release.json` (version, wire protocol, per-archive hashes, changelog notes) next to `SHA256SUMS`.
- Wire protocol number (`1`). Joining a room whose host speaks a newer or unsupported protocol now fails with a message that says which side must update.
- `playbox --version` shows the commit it was built from.
- `playbox release prepare|check|notes|manifest` for maintainers; the release workflow refuses a tag whose `Cargo.toml` version or changelog entry is missing.
- Linux confinement for the libkrun runner: per-task cgroup, rootless jail, seccomp filter, per-run network filter; confinement is recorded in the host's attestation.
- Task secrets: staged read-only at `/run/secrets`, granted per task, never written to the audit log.
- Capabilities: named operations a host offers, run once in the sandbox with bounded arguments and optional per-call owner approval (`approve`, `deny`, `calls` in the host console).
- Conversations: threads with a roster and entries signed by their authors.
- Live delivery of room messages into a running agent (`PLAYBOX_LIVE=1`), framed as untrusted.
- A black-box conformance suite (`conformance/`) that drives the built binaries, with a CI job.
- Per-room shared memory through a Hindsight sidecar; persistent room talk that survives restarts and is sent once.
- Hub: installable multi-room PWA with passkey sign-in, live events and a redesigned dashboard and cockpit.
- Research: hybrid retrieval-then-cache benchmarks and the cross-family latent "telepathy" swarm (see `steer/`).

### Changed
- CLI: typed arguments, parallel `doctor` with `--json`, readable error chains, quiet closed pipes; thin-LTO release profile.

### Fixed
- Docker leak test matches only task container names; two CI flakes in the tamper demo and quota tests.

## [0.1.0] - 2026-11-10

- First tagged release: rooms over iroh, signed grants, sandboxed tasks, audit log, the MCP server and the installer.

## [0.0.1] - 2026-11-07

- Initial tag.
