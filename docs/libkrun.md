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

## Linux confinement (`up --runtime libkrun --confine`)

Taken from the approach of Peerspan's runtime (`peerspan/runtime`, Apache-2.0; see `docs/peerspan-integration.md`). With `--confine`
(Linux only) the host does four things for every task, and refuses to start if the kernel cannot do them:

- **Jail** (`jail`): the helper moves itself into new mount, PID, IPC and UTS namespaces (and net, unless the task has a network)
  and pivots into a tmpfs root holding only the task's rootfs clone, its workspace, the secrets directory, `/dev/kvm` and the
  libraries libkrun loads. `no_new_privs`, non-dumpable. Works without root through a user namespace.
- **cgroup** (`cgroup`): a cgroup v2 per task: guest RAM + 256 MiB, 512 processes, CPU quota, swap off. Everything in it is killed
  and the cgroup removed when the task ends, however it ends; a flock finds cgroups a dead supervisor left, and host start removes them.
- **Network** (`netpolicy`): `--confine-net none|internet|internet+lan` is the most any task may have. Each task gets its own
  nftables chain matched by cgroup, a fail-closed base table rejects a runner cgroup with no chain, and loopback, link-local
  (169.254.169.254), multicast and IPv4-mapped/NAT64 aliases are rejected at every level. Where the host allows a network, a task may
  also list `allowed_hosts`: names are resolved once and pinned, so only those addresses and ports are reachable. libkrun's
  TSI is unchanged; the filter sits on the host side, so the guest never sees the difference.
- **Syscall filter** (`seccomp`): a deny-list installed after the jail and before libkrun starts a thread: ptrace, mount and the new
  mount API, namespaces, module and kexec loading, bpf, perf, key rings, reboot, swap, clock changes. Denied calls return `EPERM`.
  `--no-seccomp` turns it off for debugging.

The order inside the helper is: join the cgroup, enter the jail, install the filter, then boot. A failed step is an exit with a
message, never an unconfined VM. `Runner::attestation` (the host's signed statement) now carries `jail=…;cgroup=…;pids_max=…;
net_max=…;seccomp=deny:<hash>;helper=<hash>`, or `confine=off`, so a guest can refuse a host that is not confined.

Tested on Linux in CI (job `confine`, as root and rootless): the jail's view, the cgroup limits and cleanup, the filter's effect
(mount, ptrace, unshare, bpf refused; files, threads, sockets still work), the helper's plan order, and the network rules loaded into a
real kernel. **Not tested: a real libkrun VM under all of it.** That needs a Linux/KVM host. The most likely failure there is the
jail missing a path libkrun opens, or the filter denying a call it makes; the helper reports the failing step, and
`PLAYBOX_CONFINE_SECCOMP=off` / `--no-seccomp` separates the two.

### Secrets

`TaskSpec.secrets` names files in the host's secrets directory (`$PLAYBOX_HOME/secrets`, mode 0700, one file per secret, 64 KiB,
16 per task). The host lists which names members may use (`--allow-secret NAME`); the list goes into each member's grant, so a
request for any other name is "not authorized" before a task is queued. Values are copied into a private staging directory shown
at `/run/secrets` (read-only; for libkrun a second virtiofs share), outside `/workspace`, and removed when the task ends. The task
record and the audit log carry the names and the blake3 of each value, never the value. A task that has a secret and a network can
of course send it out: grant a secret to tasks that run code you trust, or give them no network. Works with the Docker and libkrun runners.

Not built: disk I/O limits, passt as the network path (it would replace TSI), and an allow-list seccomp measured against the libkrun
in use.

## What it does not do yet

- Tested on macOS (Apple Silicon, macOS 26, libkrun 1.19.7) only. The code builds for Linux/KVM but has not run there.
- Only `linux/arm64` or `linux/amd64` images matching the host architecture.
- No private registries or pull credentials: images come from the local Docker image store.
- No snapshot/restore, no GPU, no kernel or image measurement. A microVM here shows isolation, not attestation: the
  measurements in a host's statement name the runner but do not cover the guest kernel or image (that is what
  tier T2 evidence would add).
