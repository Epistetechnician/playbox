# The libkrun runner

Member tasks can run in a **microVM**: each task boots its own guest Linux kernel behind the hypervisor
(Hypervisor.framework on Apple Silicon, KVM on Linux), using [libkrun](https://github.com/libkrun/libkrun).
That is a stronger boundary than a container, which shares the host kernel.

```sh
brew tap libkrun/krun && brew trust libkrun/krun && brew install libkrun   # macOS; Linux: libkrun + libkrunfw
scripts/install-krun.sh          # builds playbox-krun-helper and, on macOS, signs it for the hypervisor
playbox up --runtime libkrun --repo ~/code/proj
# or, from an agent: playbox_create { "runtime": "libkrun", "repo": "..." }
```

## How a task runs

1. The image (it must be on the host's `SandboxPolicy` allowlist; default `alpine:3`) is exported from Docker
   into a cached root filesystem under `$PLAYBOX_HOME/krun/rootfs/<image id>`. Docker is needed for this step
   only, not to run the task.
2. Each task gets a private copy-on-write clone of that directory (APFS `clonefile`), deleted afterwards.
   Anything a task writes outside `/workspace` is gone when it ends; the cache is never modified.
3. `playbox-krun-helper` boots the VM with the clone as `/` and the task workspace shared at `/workspace`
   (virtio-fs). The command runs there as guest root. The helper exits with the command's exit code; its
   stdout and stderr are the guest's, kept separate.
4. The wall-clock timeout kills the helper, which ends the VM.

`preflight` boots a real VM running `true` when the host starts, so a missing library, firmware, signature or
hypervisor stops the host instead of failing on a member's first task.

## Why a separate helper process

libkrun's `krun_start_enter` never returns: it takes over the process and exits with the guest's exit code.
So every task gets its own short-lived helper (`--features krun`, so the library and the rest of Playbox build
without libkrun installed). On macOS the helper must carry the `com.apple.security.hypervisor` entitlement; an
unsigned one fails with `krun_start_enter returned -22`. `find_helper` therefore prefers the installed, signed
copy (`PLAYBOX_KRUN_HELPER`, `~/.cargo/bin`, PATH) over a freshly built one beside the executable.

## Isolation and limits

| | libkrun runner |
|---|---|
| Network | **Off by default and really off**: the VM gets no vsock device, so no network of any kind ("Network unreachable"). |
| Network on (grant allows it and the task asks) | libkrun's TSI gives the guest the host's network access with **no public-address filtering**: the guest can reach the host's loopback and LAN. Not tested further. Leave network off on hosts that matter. |
| CPU / memory | vCPUs = ceil(cpus), 1-8; RAM = `mem_mib`, at least 128. |
| Disk | The workspace share and a clone of the image; no quota. |
| Processes | Guest processes live inside the VM; there is no pid limit beyond the guest kernel's. |
| Boot | About 0.1-0.2 s after the root filesystem is cached; the first run of an image exports it (a few seconds). |
| Guest user | root inside the VM. Files it writes to `/workspace` appear on the host owned by the host user. |
| Output | Capped at 64 KiB per stream, like the other runners. |

## Linux confinement (built, not yet used by the helper)

`crates/playbox-runner` now has the pieces that close the gaps in the table above on Linux, taken from the approach of Peerspan's
runtime (`peerspan/runtime`, Apache-2.0; see `docs/peerspan-integration.md`). They are tested and **not yet called by
`playbox-krun-helper`**, so a libkrun task still runs unconfined; wiring them in needs a Linux/KVM host with libkrun, which this
project has not had.

- `jail` (Linux): moves a process into new mount, PID, IPC and UTS namespaces (and net, unless the guest keeps a network) and pivots
  into a tmpfs root holding only the paths listed, with `no_new_privs` and a non-dumpable process. Works without root through a user
  namespace. Tested as root and as an ordinary user: the jailed process cannot read a host file outside its binds, cannot write a
  read-only bind, cannot reach the host's loopback, is PID 1 of its namespace and sees no other process.
- `cgroup` (Linux): a cgroup v2 per run capping memory (guest RAM + 256 MiB), 512 processes, CPU and swap off; `kill_all` on drop;
  a flock-based way to find cgroups a dead supervisor left behind. Tested as root: limits written, a process moved in dies with the
  cgroup, stale detection.
- `netpolicy` (portable): network levels `none`, `internet`, `internet+lan`; the always-rejected ranges (loopback, link-local
  including the metadata service 169.254.169.254, multicast, reserved, IPv4-mapped and NAT64 aliases of those); and the nftables
  rules per run, with destinations pinned to resolved addresses and a fail-closed base table. The classification and rule text are
  unit-tested; **the rules have not been loaded into a kernel** (Docker Desktop's kernel has no `nft_socket`). The CI job `confine`
  runs that end to end on an Ubuntu runner and fails rather than skips there.

Not built: the seccomp allow-list for the VMM (it has to be measured against the libkrun in use), disk I/O limits, `/run/secrets`
injection, passt as the network path (it replaces TSI), and recording these settings in the attestation measurements.

## What it does not do yet

- Tested on macOS (Apple Silicon, macOS 26, libkrun 1.19.7) only. The code builds for Linux/KVM but has not run there.
- Only `linux/arm64` or `linux/amd64` images matching the host architecture.
- No private registries or pull credentials: images come from the local Docker image store.
- No snapshot/restore, no GPU, no kernel or image measurement. A microVM here shows isolation, not attestation: the
  measurements in a host's statement name the runner but do not cover the guest kernel or image (that is what
  tier T2 evidence would add).
