# v0 checklist

**v0 claim:** a stranger's agent can contribute to your repo safely, and you can prove what it did.
Anything that doesn't serve that claim is experimental or out of v0.

Legend: [x] done, [~] partly, [ ] open. Boxes last checked against the code and git history on 2026-11-13 (0.3.1 plus unreleased changes); [docs/audit-claims.md](audit-claims.md) holds the file:line evidence.

## Sandbox (modular, fail closed)
- [x] `Runner` trait declares `isolation()` (`None < Container < MicroVm < ConfidentialVm`) and `preflight()`.
- [x] Host refuses to start if its runner is below `HostConfig::min_isolation` (default `Container`) or its preflight fails. No silent fallback.
- [x] `process` runner (no isolation) needs an explicit opt-in (`PLAYBOX_ALLOW_UNSAFE_RUNNER` / test config).
- [x] Image allowlist (`SandboxPolicy`): peers cannot pick arbitrary images. Default: `alpine:3` only.
- [x] Docker hardening: read-only root fs, `/tmp` tmpfs, private IPC, no network by default, caps dropped, pids/mem/cpu/time caps.
- [x] Isolation level is part of the host's attested policy measurement.
- [~] Pin images by digest; reject tags in production mode. The allowlist accepts `name@sha256:...` entries (`SandboxPolicy`), but tags are still accepted and there is no production mode.
- [~] Rootless Docker / seccomp profile; gVisor (`runsc`) as an `Isolation::Container+` backend. Done for the libkrun helper on Linux only (`up --confine`: rootless jail, cgroup, network filter, syscall deny-list; tested in CI, not around a real guest). Docker has no custom seccomp profile and there is no gVisor backend.
- [ ] CLI/MCP flags for `--image` allowlist and `--min-isolation`. Neither flag exists; `up` and `playbox_create` take only the runtime, and the image allowlist is the default (`alpine:3`), plus the steering engine image when steering is on.
- [x] libkrun microVM backend (`Isolation::MicroVm`): `up --runtime libkrun`, run for real on a Mac ([tested-on.md](tested-on.md)); not on Linux/KVM.

### Adding a sandbox backend
1. Implement `Runner` in `playbox-runner` (`name`, `isolation`, `preflight`, `run`).
2. Register it in `runner_by_name`.
3. Never overstate `isolation()`. Hosts and attestation trust it.
4. Honour `SandboxPolicy` and the `TaskSpec` limits; the host already clamps them.

## Git and push safety
- [x] Peer pushes land under `refs/playbox/members/<id>/...`, never `main`.
- [x] Ref names validated before reaching git.
- [x] Every git call runs with hooks, fsmonitor, symlinks, submodule recursion and non-file protocols disabled; diff runs with `--no-ext-diff --no-textconv`. Tested: a host `post-checkout` hook does not run.
- [x] Host-side accept/reject (`playbox review <repo> list|show|accept|reject`) before anything reaches `main`. Accept only fast-forwards; decisions are kept under `refs/playbox/accepted|rejected/`. CLI only, on purpose: an agent steered by member text should not be able to accept member code.
- [~] Size caps on bundles and patches. A request is capped at 256 MiB, a member's pushes at 1 GiB in total (`max_push_bytes`), and a task's result at 512 MiB; there is no separate cap on a patch or a single bundle.

## Abuse limits
- [x] Per-member concurrency and rate limits, member cap, total task budget (`up --max-concurrent / --max-tasks-per-minute / --max-members / --task-budget`). The host serves one request at a time per connection, so the concurrency limit is what stops a member opening several connections.
- [x] Revocation cancels running tasks, not just future requests (also when the notice arrives by gossip). The sandbox is killed and the scratch checkout removed.

## History and attestation
- [x] Signed, hash-chained history; T0 self-attestation.
- [ ] Anchor history head in the repo (signed git note/tag) so truncation is detectable. Not built; witnessed checkpoints (`playbox_checkpoint`) cover part of this but nothing writes the head into the repo.
- [x] Platform verifiers (T1+): Secure Enclave, TPM 2.0 and Google Confidential Space ([attestation.md](attestation.md)). Without pinned evidence the tier is still T0 and is reported as such.

## Product
- [~] Two-minute happy path with recorded demo: `up` → link → `join` → agent edits → host reviews. A demo video is in `docs/demo/` and `playbox demo` exists; the video was not checked against this exact flow or its length.
- [x] Test MCP configs against real Claude Code and Codex; drop "untested". Codex, OpenCode and Hermes loaded the server and a model called it; a headless Claude Code joined a room ([adapters/README.md](../adapters/README.md)). Live delivery into a running Claude Code is not tested.
- [~] Clear errors for denied/expired grants and unreachable hosts. Errors exit non-zero with a cause chain (CLI-1) and grant failures have their own code; the wording for each case has not been reviewed.

## Docs and release gate
- [x] Threat model: malicious member, malicious host, what T0 proves ([threat-model.md](threat-model.md); it grew past two pages and is a self-review).
- [x] Wire protocol version field: `PROTOCOL` (1) in `wire.rs`, sent at join and checked both ways; the ALPN is `playbox/a2a/1`.
- [x] README "known limitations": the "What is not proven" section.
- [~] CI: tests on Linux and macOS, `cargo audit`, fuzz targets (twelve, in `fuzz.yml`), conformance, and rootful and rootless confinement are in `.github/workflows/`. The two-machine demo over the public relay is not in CI; it was run by hand ([tested-on.md](tested-on.md)).
