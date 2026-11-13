# Changelog

All notable changes to Playbox. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Edit **Unreleased** as you work; `playbox release prepare X.Y.Z` turns it into the
release section (see `docs/releasing.md`). `playbox update --check` shows the newest section to anyone running an older build.

Wire protocol: every release states its protocol number in `release.json`. A protocol bump means older builds cannot join rooms
hosted by the new one (and the reverse); a release without one stays compatible with the previous release.

## [Unreleased]

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
