# PQAI ARM64 Home-Staker Benchmark Report (Apple M4 Max, v0.4.7)

Hardware-tagged Layer 2A PQAI benchmark bundle produced by `scripts/run_pqai_arm_bench.sh` on `playbox v0.4.7`.

## Host Metadata
- **CPU:** Apple M4 Max (`arm64` / `Darwin`)
- **Cores / RAM:** 14 cores / 36,864 MB
- **Timestamp (UTC):** `2026-11-25T19:58:17Z`
- **Raw files:** `host_metadata.json`, `signatures.json`, `pcs_competitors_s.txt`, `labinius_s.txt`

## 1. Signature Ceilings (`8192` attestations / `4s` slot window, 1 core)

| Scheme | pk (B) | sig (B) | keygen (ms) | sign (ms) | verify (ms) | 8k Mbps | 8k ver (ms) | Raw OK? |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Ed25519 (classical) | 32 | 64 | 0.01 | 0.009 | 0.018 | 1.0 | 151.6 | YES |
| ML-DSA-65 (FIPS 204) | 1952 | 3309 | 0.14 | 0.133 | 0.084 | 54.2 | 691.9 | NO (agg) |
| XMSS-WOTS+-SHA256 (H=10, leanSig-style) | 64 | 2468 | 242.17 | 0.123 | 0.116 | 40.4 | 948.6 | NO (agg) |
| SLH-DSA-SHA2-128f (FIPS 205) | 32 | 17088 | 1.05 | 25.740 | 1.457 | 280.0 | 11939.5 | NO (agg) |

## 2. Hash-Based vs Lattice PCS Baseline (`2^18` witness, 1 thread, suite `s`)

| Scheme | Rate / Mode | Security | Commit (ms) | Open (ms) | Verify (ms) | Proof / Wire Size |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| binius64 BaseFold | 1/2 | 100b | 8.13 | 2.73 | 0.35 | 249.7 KB |
| binius64 BaseFold | 1/4 | 100b | 15.58 | 3.53 | 0.24 | 188.8 KB |
| binius64 WHIR | 1/2 | 100b | 8.99 | 9.39 | 1.03 | 194.8 KB |
| binius64 WHIR | 1/4 | 100b | 15.32 | 6.05 | 0.50 | 158.1 KB |
| flock-core Ligerito Fast100 | 1/2 | 100b | 5.72 | 6.90 | 0.48 | 358.6 KB |
| flock-core Ligerito Slim100 | 1/4 | 100b | 11.49 | 18.58 | 0.32 | 180.8 KB |
| Brakedown tensor | 0.581 | 100b | 8.70 | 1.06 | 1.92 | 1,698.3 KB |
| Labinius (lattice baseline) | recursion off | — | 204.38 | 11.27 | 4.15 | 591.0 KB |
| Labinius (lattice baseline) | plain-bd | — | 174.31 | 9.72 | 7.80 | 610.5 KB |

## Reproduction

```bash
./scripts/run_pqai_arm_bench.sh
```
