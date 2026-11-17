# Contributing

Thanks for helping. By contributing you agree that your work is licensed under the [Apache License 2.0](LICENSE) (section 5).

## Before you start
- Small fixes: open a pull request. Larger changes or new features: open an issue first so we agree on the shape.
- Security problems: do not file an issue; see [SECURITY.md](SECURITY.md).

## Build and test
```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
python3 ui/tests/test_hub.py        # the hub (Python 3.10+)
python3 conformance/trace.py        # every promise in docs/requirements.md has a check
```
`cargo deny check` and `cargo audit` run in CI; a new dependency must use a license in `deny.toml`.

## Pull requests
- One change per pull request, with a test. A bug fix includes the test that failed before.
- Add a line under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md) for anything a user would notice (what changed, not commit subjects).
- Changes to the wire format need a `PROTOCOL` bump (see [docs/releasing.md](docs/releasing.md)).
- A promise in `docs/requirements.md` is checked by a test carrying `REQ: ID`; keep them in step.
- Keep claims honest: if something is not proven, say so (see `docs/audit-claims.md`).

## Releases
Maintainers cut releases as described in [docs/releasing.md](docs/releasing.md): signed checksums, SBOM and build provenance.
