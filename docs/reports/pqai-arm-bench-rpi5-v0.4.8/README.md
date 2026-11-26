# PQAI ARM64 Home-Staker Benchmark Report (Raspberry Pi 5, v0.4.8)

Hardware-tagged Layer 2A PQAI benchmark bundle produced by `scripts/run_pqai_arm_bench.sh` on a bare-metal Raspberry Pi 5 (ARM Cortex-A76), compared against the Apple M4 Max bundle in `docs/reports/pqai-arm-bench-m4max-v0.4.7/`.

## Host Metadata
- **CPU:** ARM Cortex-A76 (`aarch64` / `Linux`, Raspberry Pi 5, `6.18.39+rpt-rpi-2712`)
- **Cores / RAM:** 4 cores / 8,062 MB
- **Peak RSS:** 2.70 MiB (signatures), 105 MB (Labinius lattice PCS)
- **Timestamp (UTC):** `2026-11-26T01:57:07Z`
- **Raw files:** `host_metadata.json`, `signatures.json`, `pcs_competitors_s.txt`, `labinius_s.txt`

## 1. Signature Ceilings (`8192` attestations / `4s` slot window, 1 core)

| Scheme | pk (B) | sig (B) | RPi5 keygen (ms) | RPi5 sign (ms) | RPi5 verify (ms) | M4 Max verify (ms) | RPi5/M4 ver | RPi5 8k ver (ms) | Raw OK? |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Ed25519 (classical) | 32 | 64 | 0.06 | 0.060 | 0.185 | 0.018 | 10.3x | 1514.2 | YES |
| ML-DSA-65 (FIPS 204) | 1952 | 3309 | 0.51 | 1.150 | 0.294 | 0.084 | 3.5x | 2406.6 | NO (agg) |
| XMSS-WOTS+-SHA256 (H=10, leanSig-style) | 64 | 2468 | 813.00 | 0.746 | 0.701 | 0.116 | 6.0x | 5741.6 | NO (agg) |
| SLH-DSA-SHA2-128f (FIPS 205) | 32 | 17088 | 2.66 | 72.396 | 3.793 | 1.457 | 2.6x | 31071.4 | NO (agg) |

## 2. Hash-Based vs Lattice PCS Baseline (`2^18` witness, 1 thread, suite `s`)

| Scheme | Rate / Mode | Security | RPi5 Commit (ms) | RPi5 Open (ms) | RPi5 Verify (ms) | M4 Max Verify (ms) | RPi5/M4 ver | Proof / Wire Size |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| binius64 BaseFold | 1/2 | 100b | 41.26 | 14.18 | 0.80 | 0.35 | 2.3x | 249.7 KB |
| binius64 BaseFold | 1/4 | 100b | 73.29 | 17.51 | 0.57 | 0.24 | 2.4x | 188.8 KB |
| binius64 WHIR | 1/2 | 100b | 38.09 | 50.35 | 4.14 | 1.03 | 4.0x | 194.8 KB |
| binius64 WHIR | 1/4 | 100b | 76.30 | 36.61 | 1.81 | 0.50 | 3.6x | 158.1 KB |
| flock-core Ligerito Fast100 | 1/2 | 100b | 25.06 | 27.25 | 1.23 | 0.48 | 2.6x | 358.6 KB |
| flock-core Ligerito Slim100 | 1/4 | 100b | 54.25 | 70.39 | 1.07 | 0.32 | 3.3x | 180.8 KB |
| Brakedown tensor | 0.581 | 100b | 71.72 | 4.76 | 8.56 | 1.92 | 4.5x | 1,698.3 KB |
| Labinius (lattice baseline) | recursion off | — | 975.89 | 44.43 | 16.15 | 4.15 | 3.9x | 591.0 KB |
| Labinius (lattice baseline) | plain-bd | — | 993.53 | 42.80 | 24.97 | 7.80 | 3.2x | 610.5 KB |

## Key Takeaways vs Apple M4 Max
- **Hash-based PCS verification stays far below the 100 ms home-staker ceiling on RPi5:** BaseFold verifies in `0.57–0.80 ms`, Ligerito in `1.07–1.23 ms`, and WHIR in `1.81–4.14 ms` on a single Cortex-A76 core (2.3x–4.0x slower than M4 Max).
- **Lattice PCS (Labinius) commit nears 1 second on RPi5** (`975.89–993.53 ms`, ~4.8x–5.7x slower than M4 Max) while hash-based PCS commits stay in `25–76 ms`.
- **Single-core 8,192-attestation verification separates hash-based signatures from classical:** XMSS-WOTS+ (`5.74s`) and SLH-DSA (`31.07s`) both exceed the `4s` single-core slot window on RPi5, reinforcing the need for signature aggregation on home-staker hardware.

## Reproduction

```bash
./scripts/run_pqai_arm_bench.sh
```
