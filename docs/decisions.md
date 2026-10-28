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

Open: interoperate with Peerspan (peerspan/peerspan, peerfs, runtime) or stay independent.
