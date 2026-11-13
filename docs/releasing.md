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

## Requirements and traceability

`docs/requirements.md` lists what Playbox promises, one ID each. A test or scenario that checks one carries `REQ: ID` in a comment; `python3
conformance/trace.py` (a CI step) fails when a shipped promise has no check. A release's changelog should name the IDs it adds or retires.

## Keys

`release.pub` (repository root) lists the public keys users trust, one hex key per line, newest first. It is read at build time into the
binary (`playbox update`) and mirrored in `install.sh` (`RELEASE_PUBKEY`). The secret half is the `PLAYBOX_RELEASE_KEY` Actions secret and
a 0600 copy in `~/.playbox-release/` on the maintainer's machine; nothing else holds it.

Rotate: `playbox release keygen new.key`; add the new public key as the first line of `release.pub`; set the secret to the new key; release.
Binaries built from then on trust both keys, so installs signed with the old one still update. Once no supported release is signed with the
old key, remove its line. If the secret leaks, rotate at once and say so in the changelog: releases signed with the leaked key stay valid for
binaries that still pin it, so users must reinstall with `install.sh` from a source they trust.

## What a release carries, and how to check it

- `SHA256SUMS` and `SHA256SUMS.sig`: every file below is listed, and the list is signed. `install.sh` and `playbox update` check both and refuse an unsigned release.
- `playbox.cdx.json`: a CycloneDX SBOM of every crate in the binary.
- A Sigstore build-provenance attestation for each file: `gh attestation verify playbox-linux-x86_64.tar.gz -R Epistetechnician/playbox` (add `--format json` in a script: it prints nothing on success without a terminal) proves
  the file was built by this repository's release workflow from the tagged commit, with no key of ours involved.

CI gates every push: tests, `cargo audit`, `cargo deny` (`deny.toml`), the conformance suite (including a scenario that runs the previous
release against the current build), the Linux confinement job, and fuzzing (`.github/workflows/fuzz.yml`).
