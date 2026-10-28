# v0 checklist

**v0 claim:** a stranger's agent can contribute to your repo safely, and you can prove what it did.
Anything that doesn't serve that claim is experimental or out of v0.

Legend: [x] done, [~] partly, [ ] open.

## Sandbox (modular, fail closed)
- [x] `Runner` trait declares `isolation()` (`None < Container < MicroVm < ConfidentialVm`) and `preflight()`.
- [x] Host refuses to start if its runner is below `HostConfig::min_isolation` (default `Container`) or its preflight fails. No silent fallback.
- [x] `process` runner (no isolation) needs an explicit opt-in (`PLAYBOX_ALLOW_UNSAFE_RUNNER` / test config).
- [x] Image allowlist (`SandboxPolicy`): peers cannot pick arbitrary images. Default: `alpine:3` only.
- [x] Docker hardening: read-only root fs, `/tmp` tmpfs, private IPC, no network by default, caps dropped, pids/mem/cpu/time caps.
- [x] Isolation level is part of the host's attested policy measurement.
- [ ] Pin images by digest; reject tags in production mode.
- [ ] Rootless Docker / seccomp profile; gVisor (`runsc`) as an `Isolation::Container+` backend.
- [ ] CLI/MCP flags for `--image` allowlist and `--min-isolation`.
- [ ] libkrun microVM backend (`Isolation::MicroVm`). Fits the existing trait, nothing else changes.

### Adding a sandbox backend
1. Implement `Runner` in `playbox-runner` (`name`, `isolation`, `preflight`, `run`).
2. Register it in `runner_by_name`.
3. Never overstate `isolation()`. Hosts and attestation trust it.
4. Honour `SandboxPolicy` and the `TaskSpec` limits; the host already clamps them.

## Git and push safety
- [x] Peer pushes land under `refs/playbox/members/<id>/...`, never `main`.
- [x] Ref names validated before reaching git.
- [x] Every git call runs with hooks, fsmonitor, symlinks, submodule recursion and non-file protocols disabled; diff runs with `--no-ext-diff --no-textconv`. Tested: a host `post-checkout` hook does not run.
- [ ] Host-side accept/reject (`playbox review`) before anything reaches `main`.
- [ ] Size caps on bundles and patches.

## Abuse limits
- [ ] Per-member concurrency and rate limits, member cap, total task budget.
- [ ] Revocation cancels running tasks, not just future requests.

## History and attestation
- [x] Signed, hash-chained history; T0 self-attestation.
- [ ] Anchor history head in the repo (signed git note/tag) so truncation is detectable.
- [ ] Platform verifiers (T1+). Until then say "self-attested (T0)" everywhere.

## Product
- [ ] Two-minute happy path with recorded demo: `up` → link → `join` → agent edits → host reviews.
- [ ] Test MCP configs against real Claude Code and Codex; drop "untested".
- [ ] Clear errors for denied/expired grants and unreachable hosts.

## Docs and release gate
- [ ] Threat model (1-2 pages): malicious member, malicious host, what T0 proves.
- [ ] Wire protocol version field (`v0`).
- [ ] README "known limitations".
- [ ] CI: tests on Linux and macOS, `cargo audit`, two-machine demo over the public relay, fuzz targets for wire/ref/bundle parsing.
