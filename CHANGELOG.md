# Changelog

All notable changes to Playbox. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Edit **Unreleased** as you work; `playbox release prepare X.Y.Z` turns it into the
release section (see `docs/releasing.md`). `playbox update --check` shows the newest section to anyone running an older build.

Wire protocol: every release states its protocol number in `release.json`. A protocol bump means older builds cannot join rooms
hosted by the new one (and the reverse); a release without one stays compatible with the previous release.

## [Unreleased]

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
