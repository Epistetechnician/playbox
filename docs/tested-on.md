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

**Found by this run and fixed:** Raspberry Pi OS boots with `cgroup_disable=memory`, so Docker silently ignored
the memory limit while the grant promised one. The Docker runner now refuses to start when memory, process or CPU
limits cannot be enforced (`PLAYBOX_ALLOW_MISSING_LIMITS=1` overrides). Add `cgroup_enable=memory cgroup_memory=1`
to `/boot/firmware/cmdline.txt` and reboot to enable it.

## Not yet tested

- The libkrun runner on Linux/KVM (the Pi has `/dev/kvm`, but libkrun is not packaged for it).
- A network that actually needs the relay (the two machines shared a LAN, so direct paths were also available).
- Tier T3 / confidential-VM attestation, and a hardware TPM (a software TPM and the Mac's Secure Enclave were used).
- Codex, OpenCode and Hermes driving the MCP server (only Claude Code and a scripted client were).
