# PQAI End-to-End Verification & ETH R&D Evidence Bundle (v0.4.7)

Reproducible local verification summary across all five layers aligned with Ethereum's [PQAI #0](https://blog.ethereum.org/2026/11/23/pqai-0) and Ethlabs' `pqai27` hash-only minimal-assumption roadmap.

## Claim Boundary
- **Scope:** Local engineering, formal verification, differential testing, adversarial mutation, and multi-architecture ARM64 benchmark measurements across Apple M4 Max, bare-metal Raspberry Pi 5 (`Cortex-A76`), AWS `t9g.small` (`Neoverse-V3` / Graviton5, 2 vCPU, 1.8 GB RAM), and AWS `t4g.small` (`Neoverse-N1` / Graviton2, 2 vCPU, 1.8 GB RAM).
- **Not claimed:** Official 8-vCPU AWS Graviton4 (`c8g.2xlarge`, `Neoverse-V2`) Yukon scores (`c8g.2xlarge` was not Free Tier eligible on the AWS account used, so `t9g.small` and `t4g.small` are reported as comparative Linux ARM64 measurements, not official Graviton4 scores), external audits, or mainnet/production deployment.

---

## Layer 1 — Pure Hash-Based PQAI Trust Root (`playbox`)
- **Primitives (`crates/playbox-core/src/pq.rs`):** Tri-signed custody (`Ed25519 + FIPS 204 ML-DSA-65 + FIPS 205 SLH-DSA-SHA2-128f`), anchoring long-lived roots (`RoomLog` epoch checkpoints, `BiscuitRootAnchor`, Attested Gate receipts, 5-gate bundles, releases) under pure hash-based signatures.
- **Release Root (`release.hash.pub` + `.github/workflows/release.yml`):** Offline FIPS 205 `SLH-DSA-SHA2-128f` release public key pinned (`89b904d9aeabfeee9a7c00ceeb7f22d026c5d4cf1f50f14812008b98b00dd00e`), wired into `PLAYBOX_RELEASE_HASH_PUBKEY` / `PLAYBOX_RELEASE_HASH_KEY` with fail-closed CI refusal if `SHA256SUMS.hash.sig` cannot be produced.
- **Formal proofs (`lean/Playbox/Pqai.lean`):** Kernel-checked Lean 4 theorems (`0 sorry`, checked by `lake build`) proving:
  - `anchor_cross_room_denies`, `anchor_unpinned_slh_denies`, `anchor_invalid_sig_denies`, `anchor_revoked_denies`, `anchor_revocation_monotonic`.
  - `pqai_requires_hash_sig`, `pqai_implies_hybrid`, `pqai_count_le_hybrid_count`, `pqai_checkpoint_safety` (strict majority quorum intersection `2k > n`).

## Layer 2 — EF `pq-cl`, EIP-8288 & EIP-8479 Proving Ground

### 2A. Multi-ARM64 Home-Staker Ceilings (1 thread, `scripts/run_pqai_arm_bench.sh`)
Raw hardware-tagged bundles live in:
- **Apple M4 Max** (`arm64` / macOS, 16 cores, 64 GB RAM): `docs/reports/pqai-arm-bench-m4max-v0.4.7/`
- **AWS `t9g.small` (`Neoverse-V3`, Graviton5, SVE2)** (`aarch64` / Ubuntu 24.04 Linux, 2 vCPUs, 1,791 MB RAM, `2026-11-26T03:05:02Z`, `BENCH_CPU=0`, `RAYON_NUM_THREADS=1`): `docs/reports/pqai-arm-bench-aws-t9g-neoverse-v3-v0.4.8/`
- **AWS `t4g.small` (`Neoverse-N1`, Graviton2)** (`aarch64` / Ubuntu 24.04 Linux, 2 vCPUs, 1,834 MB RAM, `2026-11-26T03:05:03Z`, `BENCH_CPU=0`, `RAYON_NUM_THREADS=1`): `docs/reports/pqai-arm-bench-aws-t4g-neoverse-n1-v0.4.8/`
- **Raspberry Pi 5 (`Cortex-A76`)** (`aarch64` / Linux, 4 cores, 8,062 MB RAM, `2026-11-26T01:57:07Z`): `docs/reports/pqai-arm-bench-rpi5-v0.4.8/`

*Note:* AWS `t9g.small` and `t4g.small` are 2-vCPU Free Tier comparative ARM64 runs (not official 8-vCPU `c8g` Graviton4 Yukon measurements).

#### Signatures (`8,192` attestations / `4s` slot window, 1 core)

| Scheme | pk / sig (B) | 8k BW (Mbps) | M4 Max ver (ms) | AWS `t9g` V3 ver (ms) | AWS `t4g` N1 ver (ms) | RPi5 A76 ver (ms) | 8k 1-core ver (M4 / `t9g` / `t4g` / RPi5) | Raw OK? |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `Ed25519` (classical) | 32 / 64 | 1.0 | 0.018 | 0.034 | 0.173 | 0.185 | 152 ms / 281 ms / 1,416 ms / 1,514 ms | YES |
| `ML-DSA-65` (FIPS 204) | 1,952 / 3,309 | 54.2 | 0.084 | 0.106 | 0.283 | 0.294 | 692 ms / 867 ms / 2,318 ms / 2,407 ms | NO (agg) |
| `XMSS-WOTS+-SHA256` (`H=10`) | 64 / 2,468 | 40.4 | 0.116 | 0.148 | 0.282 | 0.701 | 949 ms / 1,214 ms / 2,313 ms / 5,742 ms | NO (agg) |
| `SLH-DSA-SHA2-128f` (FIPS 205) | 32 / 17,088 | 280.0 | 1.457 | 1.894 | 3.564 | 3.793 | 11.94 s / 15.52 s / 29.19 s / 31.07 s | NO (roots only) |

#### Hash-Based PCS vs. Lattice PCS (`2^18` witness, suite `s`, 1 thread)

| Scheme | Rate / Mode | Security | Proof / Wire | M4 Max ver (ms) | AWS `t9g` V3 ver (ms) | AWS `t4g` N1 ver (ms) | RPi5 A76 ver (ms) | Commit ms (M4 / `t9g` / `t4g` / RPi5) |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `binius64 BaseFold` | 1/2 | 100b | 249.7 KB | 0.35 | 0.42 | 0.80 | 0.80 | 8.13 / 9.92 / 26.43 / 41.26 |
| `binius64 BaseFold` | 1/4 | 100b | 188.8 KB | 0.24 | 0.30 | 0.56 | 0.57 | 15.58 / 19.68 / 51.92 / 73.29 |
| `binius64 WHIR` | 1/2 | 100b | 194.8 KB | 1.03 | 1.55 | 3.84 | 4.14 | 8.99 / 11.83 / 31.12 / 38.09 |
| `binius64 WHIR` | 1/4 | 100b | 158.1 KB | 0.50 | 0.72 | 1.69 | 1.81 | 15.32 / 19.59 / 50.96 / 76.30 |
| `flock-core Ligerito Fast100` | 1/2 | 100b | 358.6 KB | 0.48 | 0.67 | 1.19 | 1.23 | 5.72 / 7.97 / 19.90 / 25.06 |
| `flock-core Ligerito Slim100` | 1/4 | 100b | 180.8 KB | 0.32 | 0.41 | 0.72 | 1.07 | 11.49 / 16.24 / 40.20 / 54.25 |
| `Brakedown tensor` | 0.581 | 100b | 1,698.3 KB | 1.92 | 2.98 | 7.53 | 8.56 | 8.70 / 15.14 / 42.06 / 71.72 |
| `Labinius` (lattice baseline) | recursion off | — | 591.0 KB | 4.15 | 4.14 | 11.85 | 16.15 | 204.38 / 284.82 / 668.89 / 975.89 |
| `Labinius` (lattice baseline) | plain-bd | — | 610.5 KB | 7.80 | 5.64 | 21.05 | 24.97 | 174.31 / 290.22 / 662.21 / 993.53 |

- **Peak RSS (Linux hosts):** `2.70 MiB` (`signatures`), `105–106 MB` (`Labinius`, tampered fold rejected on all 4 hosts).
- **Key multi-ARM finding:** Across all four ARM64 tiers (from M4 Max down to 2-vCPU Graviton2 `t4g.small` and bare-metal RPi5), pure hash-based PCS verification stays **< 2 ms** (`BaseFold` `0.24–0.57 ms`, `Ligerito Slim100` `0.32–1.07 ms`, `WHIR` `0.50–1.81 ms` at rate 1/4, 100-bit security) — well under the 100 ms home-staker ceiling — while `Labinius` lattice verification takes `4.14–24.97 ms` with `3x–4x` larger wire size (`591–610 KB`) and `174–994 ms` commits.
- **Multi-ARM runner:** `BENCH_CPU=0 ./scripts/run_pqai_arm_bench.sh` (defaults `BENCH_CPU=0` for 2+ vCPU Linux ARM64 hosts).

### 2B. EIP-8288 & EIP-8479 Soundness-Before-Speed Matrix (`composed-zk-benchmark-os`)
- `crates/zkbench-core/src/pqai.rs` + `examples/pqai_soundness_matrix.rs`: **14 adversarial mutation classes × 2 protocols = 28 attack cases**.
- Fail-closed reference verifier: **ADMITTED (`28/28` caught, `0` unsound accepts)**, `31.3 us` (EIP-8288) / `83.0 us` (EIP-8479).
- Unsound fast baseline: **DISQUALIFIED (`2/28` caught, `26` unsound accepts)** with timings quarantined.

### 2C. Triple-Lock `pq-cl` Verifier (`formasec`, commit `9e26fe3`)
- **Executable Python spec:** `search/pqcl_spec.py`
- **Portable Rust verifier + Kani proofs:** `pqcl-verifier/src/lib.rs` (`cargo test` 2/2, `cargo clippy --all-targets -- -D warnings` 0 warnings)
- **Kani 0.68.0 / CBMC 6.11.0 bounded model checking (`cargo kani -Z stubbing`):** **`6/6 VERIFICATION:- SUCCESSFUL`, `0` failures (`13.4s` total)** — `kani_decode_hex_32_never_panics` (unstubbed, `0/86`), `kani_quorum_never_accepts_below_two_thirds` (`0/1969`), `kani_duplicate_registry_vid_always_rejected` (`0/1987`), `kani_slot_epoch_mismatch_always_rejected` (`0/1989`), `kani_revoked_validator_always_rejected` (`0/2005`), and `kani_unsorted_or_duplicate_signers_always_rejected` (`0/1945`). Control-flow harnesses use `#[kani::stub]` deterministic zero-allocation hash oracles matching Lean's abstract hash model while production always executes real SHA-256 (verified by differential tests); compiler warnings about unreachable `caller_location`/foreign constructs are noted with 0 reachable failures.
- **Lean 4 proofs:** `lean/PqCl.lean` (`4` kernel-checked theorems, `0 sorry`, axioms `[propext, Quot.sound]`)
- **Differential + fuzz suite:** `search/tests/test_pqcl_differential.py` — `100%` exact `(ok, reason)` agreement across golden, 14 adversarial mutant classes, and 50 deterministic fuzz cases.

## Layer 3 — Air-Gapped PQAI Audit & Migration Rooms (`playbox`)
- Templates `pqai-audit` and `pqai-migrate` enforce `network: false`, empty egress/host/secret allowlists, and fail-closed refusal of runtime overrides (`crates/playbox-net/tests/pqai_conformance.rs`).
- CLI `playbox pqai scan` + `playbox pqai verify-migration --sign-key` + `playbox gate export` + `playbox gate verify --require-signature --require-pq --require-pqai` (also gated in `.github/workflows/ci.yml` `pqai-gate`).

## Layer 4 — Multi-Assumption PQAI Cryptanalysis Frontier (`composed-zk-benchmark-os`)
- `tools/pqai_frontier/frontier.py`: tamper-evident 3-front tracker separating `EvaluatorVerifiedCircuit`, `MeasuredLocalProbe`, and `AnalyticalSecurityModel`:
  1. **ECC:** Verified `ECDSA.fail` `secp256k1` circuit (`1,823,174,786` Toffoli×Qubits) + `BLS12-381` analytical scaling (`6.64B` Q×T).
  2. **Lattices / Codes:** `ML-DSA-65`, `Labinius R_648`, `Maverick dual-LPN` + live toy-LWE evaluator sanity probe.
  3. **Algebraic vs. Conservative Hashes:** Live `Poseidon2` 2-round `x^5` finite-difference degree probe (exact algebraic degree `25`) vs. live 256-bit avalanche probes for `SHA-256` (`128.84/256`) and `BLAKE2s/BLAKE3` (`128.34/256`).

## Layer 5 — Downstream Stack Integration
- **Statebook (`composed-zk-benchmark-os/crates/statebook-settlement/src/pqai_finality.rs`):** `ClassicalPreQuantum` / `PqaiReadyCalldataFallback` / `PqaiScaledHashDa` finality clock (`100/100` settlement tests passing).
- **HyperIntel (`HyperIntel/crates/hyperintel-assurance/src/pqai.rs`):** Offline hash-root PQAI anchor verifier rejecting classical, lattice-only, algebraic-hash, spoofed-alg, and tampered commitments (`8/8` tests passing).
- **hyperAI (`hyperAI/crates/hyperai-scm-adapter/src/pqai.rs`):** Pure SHA-256 WOTS+ (`w=16`, 67 chains) + EIP-8141 domain-separated offline capture verifier rejecting `eip191`/`secp256k1` under strict PQAI policy (`16/16` tests passing).

## Quick Reproduction
```bash
# Layer 1 + 3 + Lean PQAI proofs (playbox)
cd playbox/lean && lake build
cd .. && cargo test -p playbox-net --test pqai_conformance && cargo test -p playbox-cli pqai

# Layer 2B + 4 + 5 Statebook (composed-zk-benchmark-os)
cd composed-zk-benchmark-os
cargo test -p zkbench-core pqai && cargo test -p statebook-settlement
python3 tools/pqai_frontier/test_frontier.py -v

# Layer 2C (formasec, commit 9e26fe3)
cd formasec/pqcl-verifier && cargo test && cargo kani -Z stubbing --output-format terse
cd ../lean && lean PqCl.lean && cd .. && python3 search/tests/test_pqcl_differential.py -v

# Layer 5 HyperIntel & hyperAI
cargo test --manifest-path HyperIntel/Cargo.toml -p hyperintel-assurance
cargo test --manifest-path hyperAI/Cargo.toml -p hyperai-scm-adapter
```
