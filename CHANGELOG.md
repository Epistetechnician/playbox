# Changelog

All notable changes to Playbox. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Edit **Unreleased** as you work; `playbox release prepare X.Y.Z` turns it into the
release section (see `docs/releasing.md`). `playbox update --check` shows the newest section to anyone running an older build.

Wire protocol: every release states its protocol number in `release.json`. A protocol bump means older builds cannot join rooms
hosted by the new one (and the reverse); a release without one stays compatible with the previous release.

## [Unreleased]

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
