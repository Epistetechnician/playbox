# Releasing and updating

## For users

```sh
playbox update --check     # is there a newer release? (exit 10 when yes; --json for scripts)
playbox update             # download, verify, install; the old binary is kept
playbox update --rollback  # go back to it
playbox update --version 0.1.0   # a specific version (also a downgrade)
```

An update checks, in order: the archive's SHA-256 against `SHA256SUMS`; `SHA256SUMS` (and `release.json`, which it lists) against the
release signature, using the ed25519 key built into the binary; and that the new binary reports the version it should. Any failure leaves
the installed binary untouched. The swap is one `rename`, so a crash cannot leave half a binary. A build without a pinned key (a local
`cargo build`) checks hashes only and says so. A binary run from `target/` or a package manager's directory is never replaced.

`release.json` says whether the new version changes the **wire protocol**. Peers on different protocols refuse to join each other's
rooms with a message that names the side that must update; a release that keeps the protocol works in rooms hosted by the previous one.
Updating does not restart agents already running: they keep the old binary until restarted.

`install.sh` is for first installs and still works for updates (`PLAYBOX_VERSION=0.1.0 sh install.sh` pins one).

## For maintainers

1. Edit `CHANGELOG.md` under `## [Unreleased]` as you work: what a user would notice, not commit subjects.
2. `playbox release prepare X.Y.Z` bumps `Cargo.toml` and `Cargo.lock` and rolls Unreleased into `## [X.Y.Z] - DATE` (with a git-log draft
   if Unreleased was empty: edit it). Read the section, commit.
3. If a change makes older peers misread messages, bump `PROTOCOL` in `crates/playbox-net/src/wire.rs` (and `MIN_PROTOCOL` if the old
   one is dropped) and say so in the changelog. A new optional field never needs a bump.
4. `git tag vX.Y.Z && git push origin main vX.Y.Z`. The `release` workflow first runs `playbox release check` (tag, `Cargo.toml` and
   changelog agree), builds darwin-arm64, linux-x86_64 and linux-arm64, writes `release.json` (hashes, protocol, notes), signs
   `SHA256SUMS` with `PLAYBOX_RELEASE_KEY`, and publishes with the changelog section as the release notes. A tag with a `-` (for
   example `v0.3.0-rc.1`) is a pre-release and is not what `update` or `install.sh` fetch by default.

One-time: `playbox release keygen release.key` prints the public key. Store the secret as the `PLAYBOX_RELEASE_KEY` secret, the public half
as the `PLAYBOX_RELEASE_PUBKEY` repository variable (built into binaries) and in `RELEASE_PUBKEY` in `install.sh`.
