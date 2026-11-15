# Security roadmap

Where Playbox's defenses stand, what is next, and what we chose not to build. Written 2026-11-14 after comparing the
design with Anthropic's *Best Practices for CVP Participants Working with Advanced Models* (not redistributed here).
That guidance is written for offensive-security work with unsafeguarded models; we apply it in proportion to the task
(see "Proportion" below). The reasoning behind each choice is in [decisions.md](decisions.md), the attacks already tried
are in [threat-model.md](threat-model.md), and every promise has an ID in [requirements.md](requirements.md).

## Principles

1. **The agent is untrusted code.** It can be prompt-injected or wander out of scope. Contain it, then limit what contained code can reach.
2. **Controls that act before an action beat observation after it.** Allow-lists, grants and brokered tools first; logs and humans second.
3. **Fail closed.** A limit the host cannot enforce is a refusal, not a warning (decisions 13, 36, and now the network rule below).
4. **One layer is never enough.** The microVM is the strongest boundary, not the only one: signed grants, revocation, a small blast radius and an audit log sit around it.
5. **Say what is not proven.** Untested paths are listed here and in the README, not implied.

## Proportion

| Work | Isolation | Network | Process |
|---|---|---|---|
| Ordinary coding agents | microVM (or Docker where no hypervisor) | off | grants, audit log |
| Tasks that need a network | microVM on a confined Linux host | host allow-list, filtered | + egress log (planned, BOX-7) |
| Offensive-security or untrusted-model work | microVM, no network except the model API via a host-side proxy | proxy only | + escape drill, transcript retention, human supervisor |

Do not run autonomous agents against live production or high-consequence systems (OT/ICS, medical, energy). Use a replica.

## Done

| Control | Where | Requirement |
|---|---|---|
| Hardware-virtualization boundary: libkrun on Hypervisor.framework (macOS) and KVM (Linux) | `krun.rs` | |
| Network off by default; the guest gets no vsock device | decision 30 | |
| Per-task copy-on-write root; nothing outside `/workspace` survives | decision 31 | |
| Linux confinement: jail, per-run cgroup, seccomp, nftables chain per run, metadata/loopback/link-local always rejected | `confine.rs`, `netpolicy.rs` | BOX-2 |
| **A network request is refused where the host cannot filter it** (macOS, unconfined Linux) unless the owner sets `PLAYBOX_ALLOW_UNFILTERED_NETWORK` | `krun.rs` | BOX-5 |
| **Escape drill**: breakout probes run from inside a real guest | `tests/escape.rs` | BOX-6 |
| Fail closed when limits cannot be enforced; agents cannot enable the no-isolation runner | decisions 13, 36 | |
| Post-task git runs only against a recorded `--git-dir` | decision 37 | |
| Pre-auth size, time and connection caps; bridge Origin/Host checks | decisions 38, 39 | |
| Signed, hash-chained audit log that never records secrets or arguments | decision 40 | |
| Signed releases, rollback, upgrade tests | decisions, `releasing.md` | REL-* |
| Requirements register enforced in CI | `conformance/trace.py` | |

## Gaps against the CVP guidance

| Guidance | State | Next step |
|---|---|---|
| Egress blocked except the model API, through a localhost proxy with the key injected from outside | Linux confinement filters by address; macOS has no filter. No API proxy. | BOX-7: a host-side proxy and a per-task egress log |
| Pre-engagement validation: have a model try to escape, supervised and offline | Scripted probes exist (BOX-6). Run against a real guest on macOS and on nested arm64 KVM, all probes held ([tested-on.md](tested-on.md)). Bare-metal KVM and a model-driven drill not run. | Run both on a bare-metal KVM host; run the model drill below for high-risk work |
| Retain transcripts and egress logs 30 days; scan them | The audit log is tamper-evident but hash-only by design (decision 28). No egress log. | Opt-in retention mode, separate from the audit log |
| Brokered, read-only vs mutating tools | Capability calls are authorized, re-checked while running, and can need owner approval. No read-only/mutating tag. | Tag capabilities; require approval for mutating ones by default |
| Human supervision with a halt | Revocation and task stop work; there is no watcher view. | A live feed of tool calls and egress with a stop button |
| Firecracker on Linux | We use libkrun/KVM. The runner, the escape drill and the confinement around a real guest (BOX-3) passed on nested arm64 KVM ([tested-on.md](tested-on.md)); bare metal and x86-64 are untested. | Test BOX-3; evaluate Firecracker only if libkrun/KVM fails the drill |
| Hardware-pinned attestation | Software tiers only; no hardware TPM tested | Decisions 29-38 describe the path |

## Roadmap

**Now**
- Run the escape drill on a bare-metal Linux/KVM host. Done 2026-11-14 on macOS and on nested arm64 KVM ([tested-on.md](tested-on.md)).

**Next**
- BOX-7: a host-side egress proxy (model API only, key injected from outside the guest) with a per-task egress log. It replaces TSI as the way a guest reaches anything.
- BOX-3 on bare metal and in CI (done by hand on nested arm64 KVM, 2026-11-14).
- Opt-in transcript and egress retention (30 days), separate from the hash-only audit log.

**Later**
- Read-only vs mutating capability tags and default approval for mutating ones.
- A supervisor view with a halt control.
- BOX-4: measure the seccomp allow-list on a real workload.
- Multi-host revocation persistence (decision 11), an independent audit, hardware attestation roots.

**Not building**
- Per-run human review of every task for ordinary coding agents. Controls that act before an action cover it.
- Treating transcript review as a substitute for isolation.

## Escape drill

Run whenever the sandbox, helper, image, kernel or model changes.

1. **Static review.** Read the sandbox configuration: helper flags, mounts, seccomp plan, nftables rules, entitlements.
2. **Scripted probes.** `cargo test -p playbox-runner --test escape -- --nocapture`. A skip is not a pass: confirm it printed no `skipping:` line.
3. **Model drill (high-risk work only).** Offline, with no model API key in the guest, under close human supervision. Instruct the model to try to escape the guest and to stop and report if it succeeds. Review everything it tried. Any success is a release blocker and goes to the Anthropic account team if it involves an Anthropic model's out-of-scope behavior.
4. **Record** the date, host, runner version and result in `tested-on.md`.

## Review cadence

Revisit this file when a decision above reaches its "revisit when" trigger, when the threat model changes, and before exposing a host to members you do not know.
