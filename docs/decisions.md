# Decisions

| # | Decision | Why | Revisit when |
|---|---|---|---|
| 1 | iroh over rust-libp2p | QUIC-native, dial-by-key, relay fallback, 1.x stable (v1.4.1) | We need gossipsub/Kademlia semantics beyond iroh-gossip |
| 2 | Biscuit over UCAN | Datalog policy, offline attenuation, mature Rust crate; UCAN 1.0 still rc | UCAN 1.0 ratified and we want DID-native chains |
| 3 | Plain git, bundles over iroh-blobs | jj and gitoxide are pre-1.0 | Bundle sync is a bottleneck |
| 4 | Host-centered room, no global consensus | Simpler trust model; members mirror the log to detect tampering | Multi-host rooms |
| 5 | Lean models grant logic only, trusts crypto | Days not months; follows the Cedar approach | Token format is fixed |
| 6 | Pin libkrun to a release (v1.19.x) | 2.0 removes deprecated APIs | 2.0 released |
| 7 | M2 runner is Docker for now; libkrun runner is a refusing placeholder | libkrun is not installed on the dev Mac; container isolation is weaker than a microVM | A Linux/KVM or HVF-capable libkrun setup is available |
| 8 | Pushes land in per-member refs, never on `main` | Merging into `main` is a separate, later-gated act | M3 grants define who may merge |
| 9 | Grants bound to the member's iroh key via a `subject` check | A stolen grant is useless to any other key | Hosts that proxy for members |
| 10 | Over-limit task requests are refused, not clamped | Members learn their limits; the grant is the single source of truth | Never |
| 11 | Revocation is host-local and in memory; revoking an authority block id kills all attenuations | Single-host rooms need no gossip yet | Multi-host rooms or restarts that must keep bans (persist + gossip over iroh-gossip) |
| 12 | Hand-rolled MCP over stdio instead of an SDK | The surface is three methods; avoids tracking an SDK that moves fast | We need resources, prompts, or the HTTP transport |
| 13 | The unsafe no-isolation runner needs an env var on the host; agents cannot enable it by argument alone | A prompt-injected agent must not be able to switch off the sandbox | Never |
| 14 | History is a text file, one signed entry per line, locked on append | Greppable, diffable, no extra dependency | Logs get large enough to need an index |

Open: interoperate with Peerspan (peerspan/peerspan, peerfs, runtime) or stay independent.

## Workspace layer: adopt LayerFS ideas, not LayerFS (2026-10-28)

Source: Ephemeral-AI-Lab/layerfs v1.1.0 (MIT, JS/native, SQLite store, FUSE, Disposable durability, no signing, Linux mount tests waived in 1.1.0).

Adopted into `playbox-core::workspace`:
- A commit per tool call, recorded with the tool-call id; the commit's tree id is the `fs_snapshot` payload in the signed history log.
- Free fork: a branch is a pointer.
- Content-defined chunking plus content-addressed objects: an edit costs the edit, identical content is stored once.
- Refuse on conflict when both sides changed the same path; `.lww` rules (exact, `*.ext`, `dir/`) let the newest edit win.
- Append-only objects, verified on every read.

Not adopted: the SQLite store, FUSE mounting, its JS SDK and worker, and its Disposable (non-durable) profile. We add what it lacks: signed history, grants and attestation.

Revisit: if a Linux spike of LayerFS beats our module on mount speed, run it behind the same `Workspace` interface. Compare with Peerspan's `peerfs`, which already signs commits.
