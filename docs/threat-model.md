# Threat model and security review

Reviewed 2026-10-28 by reading the code with an attacker's eye and trying the attacks. Each finding below
was either demonstrated with a failing test before the fix, or checked with a test that now guards it.
This is a self-review by the authors of the code. It is not an independent audit; get one before exposing a
host to people you do not know.

## What is being protected

| Asset | Who wants it |
|---|---|
| The host machine: its files, credentials, network position, CPU, disk | any member, any stranger |
| The room's repository and what other members pushed | other members |
| The room's root signing key (it admits members and signs revocations) | anyone |
| A guest's workspace, files and agent session | a malicious host or room |
| The history log (proof of agentic history) | anyone who wants to forge or erase it |

## Who is who

| Actor | Trusted with |
|---|---|
| **Stranger**: has an iroh identity, nothing else | Nothing. May fetch the public agent card and attestation. |
| **Member**: holds a grant | Running bounded commands in a sandbox, pushing to its own namespace, pulling. *Semi-trusted*: assumed to try things. |
| **Host operator** | Everything on the host. Members must trust them with whatever they send (tasks, pushes). |
| **Guest agent** | Acts for its owner; can be steered by what it reads (prompt injection). |
| **Task code** | Nothing. It is the adversary the sandbox exists for. |

## Boundaries and what holds them

### 1. Network to host (stranger)
Anyone can dial the host. Before a connection proves itself with a valid invite or grant it may send only a
small first request (64 KiB), must send it within `idle_timeout` (30 s), and counts against a connection cap
(256 total, 8 per identity). Hardware-backed calls anyone can trigger (`GetAttestation`, `ActivateCredential`)
run at most 2 at a time. *Tests:* `a_stranger_cannot_make_the_host_swallow_a_huge_request`,
`silent_connections_are_dropped`, `the_host_caps_simultaneous_connections`,
`one_identity_cannot_take_every_connection_slot`, and `a_large_first_request_on_a_new_connection_still_works_for_a_member`
(the honest case must still work).

### 2. Authorization (member)
Grants are Biscuit tokens signed by the root key, bound to the member's iroh key, checked on every request against
facts the host supplies (time, caller, operation, requested limits). A holder may append blocks; blocks can only
restrict. *Attacks tried, all refused* (`security.rs`): supplying `req_*`, `now`, `subject` or `right` facts from a block,
re-adding a right after narrowing, reviving an expired grant, moving a grant to another key, garbage and 40 KB tokens.
Over-limit requests are refused, not clamped. One thing to know: `req_cmd` constrains only the program name (`argv[0]`),
not its arguments.

### 3. Task execution (task code → host) — the critical one
The sandbox (microVM or container) contains what runs *in* it. The host must also be careful with what it does
*after* the task, because the task controls its directory.

**Finding, critical, fixed.** After a task, the host ran `git add -A` in the task's checkout. A task could replace the
checkout's `.git` with a directory (or a `.git` file naming another directory) whose config defines a clean-filter command, plus
a `.gitattributes` selecting it. Git then ran that command **on the host, as the host user, outside the sandbox**.
Reproduced against the pre-fix code with both variants. Now the host records the real git directory when it makes the
checkout and runs every later command with explicit `--git-dir`/`--work-tree`, with global config and attributes
files off and every filter driver the host's own config defines neutralized (so a task cannot pick, say, git-lfs and
choose its input). Nested repositories with hostile config are inert. *Tests:* four exploit tests in `security.rs`.

Also checked: result size is capped before anything reads it (512 MiB, 100 000 entries; an 8 GiB sparse file is refused in
milliseconds instead of taking 30 s to hash), FIFOs and very deep nesting do not hang collection, and the workspace runner
skips symlinks and checks sizes before reading.

Sandbox properties: Docker runs with no network, all capabilities dropped, `no-new-privileges`, pid/CPU/memory limits and an image
allowlist; libkrun runs a separate guest kernel with no network device. **Found on a Raspberry Pi:** Docker silently ignored
the memory limit (the kernel boots with `cgroup_disable=memory`), so the runner now refuses to start unless memory, process and
CPU limits are enforceable.

### 4. Git data from peers
Bundles pushed by members and fetched by guests are imported, then checked with `git -c fsck.hasDotgit=error fsck --strict
--no-dangling <tip>`; a tree containing `.git/hooks/..` is rejected and the destination ref is put back as it was. The check is
explicit because `git fetch` from a bundle file does not apply `transfer.fsckObjects` (observed on git 2.43.0), so that setting
alone silently checks nothing. Minimum git behaviour relied on: `fsck --strict` reports a `.git` path component as an error and
exits non-zero (`hasDotgit`, git >= 2.2), and `fsck` accepts an object id to check what is reachable from it. Rejected objects
stay in the object store as unreachable garbage until `git gc` prunes them; they are never checked out. Guest-side `init` and `checkout` now use the same hardened git wrapper as the host (no hooks, symlinks written
as plain files). Patches a hostile host sends are refused when they try `..`, absolute paths, `.git`, or writing through a
symlink the same patch creates. Ref names are restricted to `[A-Za-z0-9._/-]`, no leading `-`, no `..`. *Tests:* `playbox-git`.

### 5. The HTTP bridge (web page in your browser → the bridge)
**Finding, high, fixed.** With no token (the default on loopback) any web page you visited could `POST` to
`http://127.0.0.1:8787/jsonrpc` as a "simple" cross-site request, or reach it by DNS rebinding, and act as the bridge's member. The bridge
now requires a JSON content type (which forces a CORS preflight it never approves), refuses any request carrying an `Origin`
when no token is set, and checks that `Host` is a loopback name. *Tests:* `playbox-bridge`.

### 6. Guest ← host (malicious host or room content)
A joining guest verifies the host's attestation before showing its invite (T0 means only "the key holder claims this";
pin the device key for T1). It cannot stop a host from returning misleading *content* to an agent. Treat everything a room
returns as untrusted input to the agent. The admission broker ([trust-layers.md](trust-layers.md): taint tracking of
member-supplied text) now exists; it was not reviewed here.

### 7. Revocation across hosts
Notices are signed by the root key and verified by every host from any source; gossip and pull carry them and are not
trusted. The set only grows. Until a notice arrives a replica still honours the grant (see limits).

## Resource abuse by members
A member can: occupy up to 8 connections, run 2 tasks at a time host-wide, and push up to 1 GiB (`max_push_bytes`, per
member, since the host started). Tasks are bounded by the grant's CPU, memory (when enforced) and time, and by a **scratch
quota** (default 1 GiB of growth and 100 000 files or directories) that is watched *while the task runs*: a task past it is
stopped, its sandbox killed (for Docker, `docker kill` is issued even though the client process is gone), its files removed,
and the member is told why. The libkrun guest's private root, which lives on the host's disk, is watched the same way.
*Tests:* `a_task_that_keeps_writing_is_stopped_at_the_quota...`, `a_flood_of_tiny_files...`, the stand-in-docker test
(`docker_cancel.rs`, which checks the kill without needing a Docker daemon) and a libkrun guest-root test.

## Audit log
Each host keeps a tamper-evident log (`<state dir>/audit.log`, or `--audit-log FILE`; `-` is stderr). One signed line per
event: joins and refusals, every denial (with the reason), pulls, pushes, tasks (runner, resources, exit code, duration, the
program name and a hash of the full command), revocations, refused connections and dropped requests. Lines are chained
(each holds the hash of the previous) and signed with the host's key, so editing, removing or inserting a line is detected by
`playbox audit <file>` / `playbox_audit`, and a host will not carry on writing into a log that fails the check.
Not in it: grants, invites, and command arguments (often secrets). *Limits:* truncating the end of the file is not detectable
from the file alone (copy the printed `head` somewhere else if that matters), someone who steals the host's key can forge
entries, and the log records what the host did, not what happened inside a sandbox.

## Known gaps (not fixed)

1. **libkrun network mode.** If a grant allows network, libkrun's TSI gives the guest the host's network access with no
   public-address filtering: it can reach the host's loopback and LAN. Network is off by default and verified off. On Linux, `up --confine` puts an nftables filter in front (loopback, link-local and, by default, the LAN are refused); that filter has run in CI but not around a real libkrun guest, and macOS has none.
2. **Disk quota is a watchdog, not a filesystem limit.** It polls every 200 ms, so a task that writes very fast can overshoot
   by what it writes in that interval (a fast disk: hundreds of MiB) before it is stopped. A size-limited filesystem for the
   scratch directory is still the stronger answer on shared hosts. The quota does not cover a container's own layer;
   Docker runs read-only with a 256 MiB `/tmp` for that reason.
3. **The libkrun helper is not itself sandboxed on macOS** (it runs as the user; the VM is the boundary). A VMM bug is a
   host compromise. On Linux `up --confine` adds a jail, a cgroup and a syscall filter around the helper (docs/libkrun.md), tested in CI but not around a real libkrun guest; a `sandbox-exec` profile for macOS is not built.
4. **Members can read the whole repository.** `pull` accepts any ref name, so a member can fetch any branch, tag, or
   another member's pushed branch. Share a repository made for the room, never one with history you need private.
5. **The audit log cannot show truncation or a stolen key** (see above), and nothing ships it off the machine.
6. **Root key is a plain 0600 file on one machine,** with no rotation or backup. If it is lost nobody can admit members or
   revoke; if it is stolen, all of them can.
7. **Grants cannot be renewed.** An invite is single-use, so a member whose grant expires (default 1 h) must be re-invited.
8. **Attestation is self-reported at T0.** A malicious host signs whatever it likes. Hardware evidence helps only with a pinned
   device key (trust on first use is a risk at first contact). The T3 verifier (Google Confidential Space) exists and has run once against a real TDX VM; no other confidential-VM kind has one.
9. **Revocation is eventually consistent.** A replica cut off from every peer keeps honouring a grant until it expires.
10. **Public metadata.** The agent card and attestation reveal the host's binary hash, runner, policy hash and OS to anyone.
11. **A guest agent can be steered** by room content. Its human's approval prompts are the main defence today.

## Dependencies

`cargo audit` (RustSec) reports, as of this review:

| Advisory | Crate | Reality here |
|---|---|---|
| RUSTSEC-2023-0071 (Marvin timing attack, no fix exists) | `rsa` 0.9 | Used only to *encrypt* to a TPM's public endorsement key (public-key operation). The attack targets private-key operations. Low. |
| RUSTSEC-2026-0387 (timing variability) | `crypto-bigint` 0.5 (via `rsa`, `p256`) | We verify signatures and do one ephemeral ECDH; no long-lived secret passes through it. Low; upgrade when `p256`/`rsa` release versions using `crypto-bigint` 0.7. |
| RUSTSEC-2024-0436, RUSTSEC-2026-0173 (unmaintained) | `paste`, `proc-macro-error2` | Compile-time macros reached through iroh and biscuit. No runtime exposure. |

The first two are in `.cargo/audit.toml` and all four in `deny.toml` (which CI's cargo-deny reads), so a clean `cargo audit` in CI means "nothing new", not "nothing". Our
own code has `unsafe` only in the libkrun FFI helper, the Linux jail and syscall filter (`jail.rs`, `seccomp.rs`, libc calls) and the CLI's SIGPIPE reset in `main.rs`.

## Not covered by this review

The TPM / Secure Enclave verification code and the new admission-broker modules were written by other sessions and read only for how
they are reached from the network. The Lean model proves properties of the grant *logic* and trusts the cryptography.
**Fuzzing** (see `fuzz/README.md`): this review ran eight libFuzzer targets; there are now twelve (added: capability binding, room state files, latent envelopes and the latent store; each ran 60 s with no crash on 2026-11-13, [proof-status.md](proof-status.md)). The eight cover the request frame and A2A message, grant and invite tokens,
revocation notices, attestation statements, history and audit-log files, links/PEM/peer addresses, TPM and X.509 evidence,
and the Confidential Space token. Each ran 75-120 s (roughly 90 000 to 560 000 inputs); none produced a crash, hang or
out-of-memory. A 15 000-request mutation barrage against a live host, partly from a real member's identity so it reached the
pull, push and task handlers, left it answering with well-formed replies and an audit log that still verifies, and 120 damaged
git bundles never hung the host or corrupted its repository. Minutes of fuzzing is a smoke test, not assurance: run the targets
for hours before relying on this, and note that `git` itself (which parses bundles and patches) was not fuzzed.

## Suggested next steps, in order

1. Independent review of the sandbox boundary (section 3) before hosting people you do not know.
2. Ship audit heads (or the log) to a second place, and add a filesystem-level size limit for scratch space.
3. Run the fuzz targets for hours in CI, and `cargo audit` and the exploit tests on every change.
4. Sandbox the libkrun helper; replace TSI with a filtered network backend.
5. Per-ref allowlists for `pull`, grant renewal, and root-key backup/rotation.
