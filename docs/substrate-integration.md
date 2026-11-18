# Agent Substrate (agent-substrate/substrate): can a playbox host run on it?

Tried on 2026-11-17: Substrate 1.0 (shallow clone of `main`) on a local `kind` cluster, Docker Desktop on an Apple Silicon Mac, gVisor sandbox class, Envoy dataplane. Playbox: a linux/arm64 build of `playbox-cli` from this tree (reports 0.3.6). The Mac guest was playbox 0.3.14. One run each; nothing here is a benchmark.

## What Substrate is

A Kubernetes runtime for many sandboxed, mostly idle agents (Go, Apache-2.0, CNCF Sandbox). Actors run in gVisor or Cloud Hypervisor microVMs, are snapshotted when idle and restored on a request, get a SPIFFE identity, and have all traffic mediated: no actor-to-actor path, ingress through one HTTP(S) router, egress through a gateway with a per-actor policy. It does not do cross-owner collaboration, signed grants, or verifiable history.

## Result

| Question | Result |
|---|---|
| Does `playbox up` start in an actor? | Yes, after one policy change (below). |
| Can a guest outside the cluster join it? | Yes. A Mac guest joined over the n0 relay and ran `echo` on the host. |
| Does the host survive suspend and resume (memory snapshot)? | The host answered over the relay afterward: a second join with the used link got `invite already used or unknown`, a reply from the host. Room state and the audit chain were not checked; no unused invite was left. |
| Can the host run tasks inside the actor? | **No.** |

### Egress: use `tls_passthrough`, not `https`

An `https` rule makes the gateway intercept TLS and re-sign it with its own CA. Playbox's iroh client does not trust that CA, so `playbox up` failed with `could not reach the relay for transport iroh within 20s`. This policy works:

```yaml
rules:
- http: {hostnames: ["*"], ports: {numbers: [80]}}
- tls_passthrough: {hostnames: ["*"], ports: {all: {}}}
```

(`*` on both rules on every port is rejected as a tie.) Narrow `hostnames` to the relay and discovery hosts you use. All other UDP is dropped, so iroh falls back to the relay; direct hole-punched paths do not exist from an actor.

### Tasks cannot run inside a gVisor actor

| Runner | Result |
|---|---|
| `docker` | `docker is not installed`. The actor also has no `/var/run/docker.sock`. |
| `libkrun` | `playbox-krun-helper not found` (not in my image). The actor has no `/dev/kvm`. |
| `process` | Refused: the host's isolation floor defaults to Container (`crates/playbox-net/src/host.rs`, `min_isolation`) and no CLI flag lowers it. Not run. |

Inside the actor: uid 0 with only `CAP_AUDIT_WRITE`, `CAP_KILL`, `CAP_NET_BIND_SERVICE`; `unshare` and `mount` fail with `Operation not permitted`, so `--confine` is out as well. Substrate's microVM class provides `/dev/kvm` but needs nested-virtualization nodes (not available on Docker Desktop for Mac). I did not try it.

## What is not established

- The microVM class (libkrun or Docker inside it).
- Long-running hosts: the attestation token expires; one suspend/resume cycle only.
- Room state and audit-chain continuity across resume.
- A resumed host with in-flight guest connections.
- Any hardware other than arm64 on kind.

## If this is pursued

The fit is a Playbox runner backend that launches each task as a sibling Substrate actor and returns the result, with gVisor as the sandbox. That is a new runner against Substrate's actor API, not a packaging change. The host itself can run as an actor today under the egress policy above. Do this only if there is a real need to host many rooms on Kubernetes.

Substrate's own pieces that overlap Playbox's `playbox-net` vetting work: the egress gateway injects credentials outside the sandbox, so an actor never holds secrets (`docs/guides/egress-credential-injection.md` in its repo).
