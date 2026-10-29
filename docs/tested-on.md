# What has been tested, and where

Automated tests (`cargo test`, 145 on the last run) run on one machine. This page records the runs that
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

**Found:** `playbox shell` could not take a multi-line capability, so `key=@file` argument values were added. Invites are single-use, so a
guest whose session ends must be given a new link, and a guest's workspace belongs to its member id.

## Not yet tested

- The libkrun runner on Linux/KVM (the Pi has `/dev/kvm`, but libkrun is not packaged for it).
- A network where direct paths are impossible (the India run used a relay-only link, but direct paths were not blocked).
- Tier T3 / confidential-VM attestation, and a hardware TPM (a software TPM and the Mac's Secure Enclave were used).
- Codex and OpenCode driving the MCP server (Claude Code, Hermes and a scripted client have).
