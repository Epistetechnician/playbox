# Flashbots TEE stack: integration design note

Status: design note + local phase-1 prototype (image not yet built). Date: 2026-11-17.

## Repos

| Repo | What it is | Playbox role |
|---|---|---|
| [flashbots-images](https://github.com/flashbots/flashbots-images) | mkosi + Nix reproducible, minimal, hardened Debian images for Intel TDX (profiles: azure, gcp, devtools; `images/*.conf`) | Build measured images for trust-sensitive services. Vendored locally at `vendor/flashbots-images` (commit `b62ed41`, currently gitignored). |
| [attested-tls](https://github.com/flashbots/attested-tls) | Rust crates binding TEE attestation evidence to TLS certs (`attested-tls`, `nested-tls`) | Native attested transport in `playbox-core`, later. |
| [attested-tls-proxy](https://github.com/flashbots/attested-tls-proxy) | Binary: HTTP reverse proxy and TCP/gRPC tunnel over attested TLS; measurements-file policy; `X-Flashbots-Measurement` header | Attested transport now, no core changes. |
| [panetiere](https://github.com/flashbots/panetiere) | Anonymous broadcast: clients publish one message each, threshold servers reveal only the sum. Not audited. | Spike only: anonymous room input. |

## Not a fit

GPU training/eval nodes (`steer/tools/node/*.sh`, `bootstrap.sh`). They are rented boxes provisioned over SSH with CUDA, Python and HF caches. The images are minimal, read-only, no SSH, no GPU drivers. Out of scope.

## Goal

Let a room member verify, at join time, that the gate, judge, holdout/evaluator service, or a room host is running an exact, reproducibly built image, instead of trusting its operator. Ties to the host-less room work (ROOM-5 to ROOM-17) and the gate as room evaluator.

## Architecture

```
member --HTTP--> attested-tls-proxy client (localhost)
                    | attested TLS (verifies measurements file)
                    v
        [TDX CVM built from flashbots-images]
          attested-tls-proxy server --HTTP--> gate / judge / holdout service
```

- **Image**: `vendor/flashbots-images/images/playbox-gate.conf` (includes `shared/mkosi.conf` and `modules/playbox-gate/mkosi.conf`). Contains the Tier A Python gate (`challenge/gate_http.py` + `gate.py`) and `attested-tls-proxy` server (no judge binary yet). Image hash is the pinned identity.
- **Transport**: proxy server inside the CVM, proxy client on the member's machine. Services stay plain HTTP. No `playbox-core` changes in phase 1.
- **Policy**: a measurements JSON (accepted attestation type + measurements or OS image hashes) shipped with the room invite and checked by `playbox join` consent.
- **Evidence**: record the verified `X-Flashbots-Measurement` value with gate and holdout results in the room log, so a result is attributable to an image hash.
- **Room log replication between hosts**: use the proxy's TCP tunnel (gRPC-capable) in phase 1; one attested connection per source connection.

## Phases

1. **Local, no TDX hardware.** Build `playbox-gate.conf` via the Lima/QEMU path. Run proxy with `--allowed-remote-attestation-type none`. Goal: pipeline works end to end. Exit: gate reachable through the proxy; measurement header present (or absent as expected under `none`).
2. **Real TDX.** Deploy to Azure or GCP TDX (existing profiles). Pin measurements file. Exit: join fails on a modified image, passes on the pinned one.
3. **Room integration.** Measurements file in invite; `join` shows the image hash in consent; room log stores measurement evidence; conformance test for a tampered image.
4. **Native transport (optional).** Use `attested-tls` / `nested-tls` in `playbox-core` to drop the proxy hop. Nested TLS fits failover: stable outer CA identity, inner attested identity per CVM lifetime.
5. **Panetière spike** under `spikes/`, only with a concrete use (blind votes, anonymous holdout submissions). Needs authenticated consistent broadcast, which the signed room log could supply. Max about 300 clients per round with defaults. Unaudited: no production use.

## Risks and open questions

- **License**: flashbots-images shows no LICENSE file at its root. Confirm terms before redistributing or committing the vendored copy. The other three ship a LICENSE.
- **Hardware**: no known TDX hosts on our side; phase 2 needs cloud TDX VMs and cost sign-off.
- **Packaging**: unverified that gate/judge (Python/Rust mix) run in a stripped Debian image; may need a static Rust build or a Python layer in the image.
- **Reproducibility**: upstream claims reproducible builds; we must verify the same hash across two builds of `playbox-gate.conf` before pinning.
- **Trust anchor**: the measurements file is itself trusted input; its distribution (invite, signed room log) must be authenticated.
- **Proxy limits**: HTTP-centric; extra hop for non-HTTP traffic; 60 s connection setup deadline.
- **Maturity**: all four are upstream reference code; pin commits, review before depending.

## Next steps

1. Decide whether to commit the vendored `vendor/flashbots-images` (license check first).
2. Build `playbox-gate.conf` with mkosi/Lima and run phase 1.
3. Vendor or pin `attested-tls-proxy` for phase 1 testing.

## Status (2026-11-17)

- Done: `challenge/gate_http.py` (loopback HTTP front for the Tier A gate, with tests); `vendor/flashbots-images/modules/playbox-gate` + `images/playbox-gate.conf` (proxy pinned to `95eb432`, gate sources, two systemd units). Gate and wrapper smoke-tested in a staged copy of the image layout (a cheating scheme is rejected, receipts verify).
- Not done: the mkosi image build itself (mkosi/nix not installed here; Lima instance `pbkvm` unreachable from the sandbox); TLS key/cert provisioning for the proxy; real `measurements.json`; a writable `/var/lib/playbox-gate` on the read-only image is assumed, unverified.
