# What has been tested, and where

Automated tests (`cargo test --workspace`; 494 `#[test]` functions in the Rust source, plus Python suites and the conformance scenarios) run on one machine. This page records the runs that
used real second machines or hardware, so claims in the other docs can be weighed.

## Two machines: Mac (Apple Silicon, macOS 26) and Raspberry Pi 5 (Debian 13, aarch64)

Same LAN, public n0 relays. In each run the link or peer address given to the other machine had its direct IP
addresses removed, so it could only be reached through the relay.

| Run | Result |
|---|---|
| Pi hosts (Docker sandbox), Mac joins and runs a task | Worked. The task failed `test.sh`, a fix was applied, and the re-run passed: 1.2 s end to end. |
| Mac hosts (libkrun microVM), Pi joins and runs a task | Worked. The guest kernel was Linux 6.12, the network was unreachable, and the Pi's own `uname` was never what ran. 2.8 s. |
| Mac primary + Pi replica, guest uses the Pi, primary revokes | Worked. The guest switched hosts in 0.3 s, pinged, pulled the Pi's repo and ran a task in the Pi's Docker. After the revocation the Pi refused the grant about 10 ms later, and its state held the signed notice. |
| Mac hosts (Docker), Hermes v0.21.0 on the Pi joins through the n0 relay only (direct IPs stripped from the link), pulls, delegates a fix and applies the patch | Worked. 27 s for the whole agent run (model: gpt-6-luna). The agent's first command failed (exit 127 on the host) and it recovered with a second one; the host audit log shows join, pull and both tasks. The Pi's default Hermes model was rejected by its Codex login, so the run overrode it with `--provider openai-codex -m gpt-6-luna`. |

**Found by this run and fixed:** Raspberry Pi OS boots with `cgroup_disable=memory`, so Docker silently ignored
the memory limit while the grant promised one. The Docker runner now refuses to start when memory, process or CPU
limits cannot be enforced (`PLAYBOX_ALLOW_MISSING_LIMITS=1` overrides). Add `cgroup_enable=memory cgroup_memory=1`
to `/boot/firmware/cmdline.txt` and reboot to enable it.

## One machine, real iroh network: IP-free, key-bound link

`playbox up --for-guest <id>` on the public n0 network: the link held only a relay address (no `Ip` entries) and was
marked bound. A different key was refused (`this invite is for a different key`) without spending the invite; the
bound guest then joined; reusing the link was refused. All four are in the host's audit log. Host and guest shared
one machine, so this shows the link works without IPs, not that two separate networks can only meet via the relay.

## Mac host and a friend's agent in India (2026-10-29)

Mac (Docker sandbox, iroh transport, named room) hosted; a friend's agent on another network joined from a link.
Each run is in the host's audit log (valid hash chain, 71 entries at the end).

| Run | Result |
|---|---|
| Join, pull, task, push over the internet | Worked. The pushed branch waited under `refs/playbox/members/` and never moved `main` until reviewed. |
| Relay-only join (link with the direct IPs removed) | Worked: join 0.3 s, ping 9 ms, pull 0.8 s, task 1.5 s. The audit log cannot show whether a direct path was used afterwards. |
| Fix loop: check fails on `main`, fix applied with `apply`, branch pushed | Worked. The same check passed on the pushed ref on the host. The branch was not a fast-forward because `main` moved meanwhile, so `review accept` refused it (rejected). |
| Sandbox attacks (from India and from local guests) | Contained: non-root container, no `/Users`, no network, fork bomb refused, memory hog killed (137), timeout enforced, over-grant timeout/network/memory refused by the grant. |
| Revocation | A revoked member's ping, pull and task were refused (`grant revoked`) and logged. |
| Injection guard (`PLAYBOX_APPROVERS` set) | A command copied from another task's output was refused; `playbox approve` capability worked once; reuse and use for a different command were refused. |
| Memory quarantine (two homes) | Other author's entry stayed quarantined until a pinned reviewer accepted it; an unpinned reviewer was refused. |
| Host identity (`--min-host-tier t1`, Mac Secure Enclave) | With the device key unpinned the join was refused (host achieves T0); after `playbox pin add secure-enclave <fp>` the same link joined at verified T1. Then repeated by the friend's agent in India after pinning the key (sent outside the link): joined at verified T1, notes "device key is pinned". |
| Steering room (GPT-2 in Docker on the Mac, layer 6, sentiment direction), friend's agent over a relay-only link | Worked: two steered runs and a probe, each `Completed`. The spec digests (`83fa65c5…`, `fdc2eb34…`) and weights hash (`282acf3d…`) equal those of my local guest's runs of the same prompts, so both sides applied the same edit to the same model. Receipts are signed by the host; the generated text is untrusted. |
| Real task: friend's agent writes unit tests for `words`/`parse_args` in `session.rs`, host reviews and tests | Worked. One commit touching only a `#[cfg(test)]` module, pushed from India; the host read the diff, ran `cargo test -p playbox-cli` in a throwaway worktree (8 passed), then fast-forwarded `main` with `playbox review accept`. Delegated tasks have no toolchain or network, so the host ran the tests, not the sandbox. |

**Found:** `playbox shell` could not take a multi-line capability, so `key=@file` argument values were added. Invites are single-use, so a
guest whose session ends must be given a new link, and a guest's workspace belongs to its member id.

## Google Confidential Space (Intel TDX), 2026-10-29

A `c3-standard-4` Confidential Space VM produced a real attestation token, which the verifier checks (fixture in `crates/playbox-core/tests/fixtures/confidential-space`).
A Playbox host then ran as the VM's workload with `PLAYBOX_ATTEST=tdx`, and a guest on a Mac, across the internet through n0's relay, joined with `min_host_tier: T3`
and verified the host at T3 (its image digest was on the guest's allow-list). Two bugs found and fixed: Google's hardware model string `GCP_INTEL_TDX`, and HTTP
chunked encoding from the launcher socket. Not tested: a real token with the wrong digest or project (a fixture covers it), and long-running hosts (the token expires).
Details: [trust-layers.md](trust-layers.md).

## Agents driving the MCP server (2026-10-29)

Codex 0.160.1, OpenCode 1.19.4 and Hermes 0.22.5 each called `playbox_status` through the real agent's MCP loader, and the server's history log shows the call; a headless
Claude Code joined a room and ran a task as a second member ([adapters/README.md](../adapters/README.md)). Not tested: a running Claude Code receiving live-delivery
notifications ([live-delivery.md](live-delivery.md)).

## Escape drill (2026-11-14, macOS arm64, Hypervisor.framework)

`cargo test -p playbox-runner --test escape` against a real libkrun guest with the signed helper: 13 probes (host paths, SSH keys, Docker socket, `/sys` and `/dev/mem`, loopback, metadata service, the internet, secrets, workspace parent) all failed to get out, and the two canaries that must fire did (one proves the probes ran, one that the guest has the `wget` the network probes use). Re-run the same day after the host-home probe was changed to the host user's own `$HOME/.ssh` and the `wget` canary was added: same result. The task is root inside its own guest kernel, so mounting filesystems and writing the guest's `/proc/sys` succeed; those are not host escapes. The model-driven drill ([security-roadmap.md](security-roadmap.md)) has not been run.

**Linux/KVM, the same day.** The same command on nested KVM (Ubuntu 24.04 arm64, kernel 6.8, in a Lima VM on an M4 Max; libkrun 1.19.7, libkrunfw 5.6.2): both tests ran with no skip and passed, and the eight `tests/krun.rs` tests passed too (a real VM boots, no network unless asked, timeout, root quota, workspace share). This is nested, not bare-metal, KVM.

**The confinement around a real guest (BOX-3), same VM, as root.** `tests/confine_guest.rs` with `PLAYBOX_CONFINE_REQUIRE_ALL=1`: with the jail, run cgroup, syscall filter and network filter all on, a guest boots, runs, keeps separate streams and its exit code, and writes its workspace; a task without a network has none; a task with one connects to a public address but not to a listener on the host's loopback or to 169.254.169.254. The control in the same test, the same task on an unfiltered runner, does reach that loopback listener, so the filter is what stops it. The CI `confine` suite (9 cases, as root) also passed on this kernel. Not run: x86-64, bare metal, `internet+lan`.

Three things this run found, all fixed the same day and re-run: (1) the guest's `/etc/resolv.conf` was empty, so a task with a network could not resolve names; it now gets a resolver or, with a host list, the pinned addresses in `/etc/hosts` (BOX-8), and the suite checks that a name resolves, that a listed host is reachable by name and that an unlisted address is not. On macOS `a_guest_with_a_network_can_resolve_names` passed with the owner's opt-in set. (2) `--confine` as root failed on a machine that had not run Playbox since boot, because `/sys/fs/cgroup/playbox` did not exist; the host now creates it. (3) The seccomp case of `tests/confine.rs` wrote a fixed file in `/tmp`, so running the suite as one user and then another failed; the name now carries the process id.

Still open from this run: on Ubuntu 24.04 the rootless jail cases fail (`setgroups: Permission denied`; `kernel.apparmor_restrict_unprivileged_userns=1`). As root everything passes. CI runs them on Ubuntu 22.04, where they pass. None of this runs in CI, which has no libkrun; it skips there.

Re-checked on Ubuntu 24.04.4 (kernel 6.8.0-134, Lima VM): the unshare succeeds, but AppArmor strips the new namespace's capabilities from an unprofiled process, so the `setgroups`/`uid_map` writes get EPERM. The jail code cannot get around that from inside the process. With `kernel.apparmor_restrict_unprivileged_userns=0` the two rootless cases pass (the setting was restored afterwards). The jail now names this cause in its error. To run the rootless cases on 24.04, either set the sysctl to 0 on that host, or give the binary an AppArmor profile that has `userns,`.

## Clean install and demo (2026-11-14, new Ubuntu 24.04 arm64 VM, nothing installed)

Following the README only. `install.sh` installed the signed 0.3.2 release and `playbox up --local` / `join --local` worked with no toolchain. From a fresh clone, `cargo test --workspace` first failed twice: no C compiler (the README did not say one is needed), then the rootless jail cases under AppArmor (above). With both fixed it passes: 62 test binaries, 0 failed. `scripts/demo.sh` passes all six steps there and on macOS ([demo.md](demo.md)). Also found: the F3 dry run no longer verified because the protocol file had been amended after it (fixed, amendment A4); verifying the climbmix runs needs their data files and ends in a Python traceback without them, not a clean failure (open); `playbox doctor` reported the research venvs as ready on a machine that only had the repository mounted (open).

## Not yet tested

- The libkrun runner and its confinement on bare-metal or x86-64 Linux/KVM. Both passed on nested arm64 KVM (above). The Pi has `/dev/kvm`, but libkrun is not packaged for it.
- A network where direct paths are impossible (the India run used a relay-only link, but direct paths were not blocked).
- A hardware TPM (a software TPM and the Mac's Secure Enclave were used), and a confidential VM other than Google Confidential Space.
- Cloudflare Access in front of a named tunnel, and Web Push to real Apple, Google or Mozilla services ([ui/README.md](../ui/README.md)).
