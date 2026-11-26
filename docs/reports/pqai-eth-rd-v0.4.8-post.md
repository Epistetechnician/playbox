# PQAI Home-Staker ARM64 Ceilings, EIP-8288/8479 Soundness Matrix, and Formally Verified `pq-cl` Verifier (v0.4.8)

*Draft ready for Ethereum Magicians / ETH R&D (`pq-cl` / EIP-8288 / EIP-8479). All numbers are reproducible local/staging measurements; no mainnet deployment or external audit is claimed.*

## TL;DR

We benchmarked pure hash-based PCS and hash-based signatures across four real ARM64 machines (Apple M4 Max, AWS `t9g.small` Neoverse-V3, AWS `t4g.small` Neoverse-N1, and bare-metal Raspberry Pi 5 Cortex-A76), built a 28-mutant soundness-before-speed suite for EIP-8288 and EIP-8479, and mechanically verified a `pq-cl` validator key-registry + aggregation verifier across four independent layers (Python executable spec, Rust verifier, Lean 4 kernel proofs, and Kani/CBMC bounded model checking).

1. **Hash-based PCS verification easily clears home-staker ceilings — even on a Raspberry Pi 5:** At 100-bit security and `2^18` evaluations (1 thread), `BaseFold`, `Ligerito Slim100`, and `WHIR` verify in **`0.24–1.81 ms`** (`158–189 KB` proofs), outperforming the lattice baseline (`Labinius`: `4.14–24.97 ms` verify, `174–994 ms` commit, `591–610 KB` wire, `106 MB` RSS).
2. **Unaggregated hash signatures blow past home-staker budgets, proving EIP-8288 aggregation is mandatory:** At 8,192 attestations per 4s slot, raw `XMSS-WOTS+` requires `40.4 Mbps` (`5.74 s` 1-core verify on RPi5) and `SLH-DSA-SHA2-128f` requires `280 Mbps` (`11.9–31.1 s` 1-core across all four chips). `SLH-DSA` belongs on long-lived roots/checkpoints, not per-slot validator attestations.
3. **Soundness before speed:** Our 28-mutant EIP-8288/8479 suite admits a fail-closed reference verifier (`28/28` caught, `31.3 us` / `83.0 us`) and disqualifies a faster check-skipping baseline (`2/28` caught, timings quarantined).
4. **Triple-lock `pq-cl` verifier:** Python spec ↔ Rust verifier achieve `100%` exact `(ok, reason)` parity across golden + 14 adversarial mutant classes + 50 fuzz cases, backed by `4/4` Lean 4 theorems (`0 sorry`, axioms `[propext, Quot.sound]`) and **`6/6` Kani 0.68.0 / CBMC 6.11.0 harnesses verified (`0` failures)**.

---

## 1. Four-Way ARM64 Home-Staker Benchmarks (1 Thread)

Hardware bundles committed in `playbox/docs/reports/pqai-arm-bench-*-v0.4.{7,8}/` via `BENCH_CPU=0 ./scripts/run_pqai_arm_bench.sh`. *(AWS `t9g.small` and `t4g.small` are 2-vCPU Free Tier comparative Linux ARM64 runs, not official 8-vCPU `c8g` Graviton4 Yukon scores.)*

### Signatures (`8,192` attestations / `4s` slot, 1 core)

| Scheme | pk / sig (B) | 8k BW (Mbps) | M4 Max (ms) | AWS `t9g` V3 (ms) | AWS `t4g` N1 (ms) | RPi5 A76 (ms) | Raw Feasible? |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `Ed25519` (classical) | 32 / 64 | 1.0 | 0.018 | 0.034 | 0.173 | 0.185 | YES |
| `ML-DSA-65` (FIPS 204) | 1,952 / 3,309 | 54.2 | 0.084 | 0.106 | 0.283 | 0.294 | NO (agg) |
| `XMSS-WOTS+-SHA256` (`H=10`) | 64 / 2,468 | 40.4 | 0.116 | 0.148 | 0.282 | 0.701 | NO (EIP-8288) |
| `SLH-DSA-SHA2-128f` (FIPS 205) | 32 / 17,088 | 280.0 | 1.457 | 1.894 | 3.564 | 3.793 | NO (roots only) |

### Hash-Based PCS vs. Lattice Baseline (`2^18` witness, suite `s`, 1 thread)

| Scheme | Mode / Sec | Proof / Wire | M4 Max ver (ms) | AWS `t9g` V3 ver (ms) | AWS `t4g` N1 ver (ms) | RPi5 A76 ver (ms) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `binius64 BaseFold` | 1/4, 100b | 188.8 KB | 0.24 | 0.30 | 0.56 | 0.57 |
| `flock-core Ligerito Slim100` | 1/4, 100b | 180.8 KB | 0.32 | 0.41 | 0.72 | 1.07 |
| `binius64 WHIR` | 1/4, 100b | 158.1 KB | 0.50 | 0.72 | 1.69 | 1.81 |
| `Brakedown tensor` | 0.581, 100b | 1,698.3 KB | 1.92 | 2.98 | 7.53 | 8.56 |
| `Labinius` (lattice baseline) | rec-off / plain-bd | 591–610 KB | 4.15–7.80 | 4.14–5.64 | 11.85–21.05 | 16.15–24.97 |

---

## 2. EIP-8288 & EIP-8479 Soundness-Before-Speed Matrix (`composed-zk-benchmark-os`, branch `pqai-layers-2-4-5`)

- `crates/zkbench-core/src/pqai.rs` + `examples/pqai_soundness_matrix.rs`: 14 adversarial mutation classes × 2 protocols (`28` mutants).
- `pqai_reference_fail_closed_v1`: **ADMITTED** (`28/28` caught, `0` unsound accepts; `31.3 us` EIP-8288, `83.0 us` EIP-8479).
- `fast_skip_checks_baseline_unsound`: **DISQUALIFIED** (`2/28` caught, `26` unsound accepts; timings quarantined).

---

## 3. Mechanically Verified `pq-cl` Verifier (`formasec`, commit `9e26fe3`)

- **Executable Python spec:** `search/pqcl_spec.py`
- **Rust verifier:** `pqcl-verifier/src/lib.rs` (`cargo test` 2/2, `cargo clippy --all-targets -- -D warnings` clean)
- **Differential & fuzz parity:** `search/tests/test_pqcl_differential.py` (`100%` exact `(ok, reason)` agreement across golden + 14 mutant classes + 50 fuzz cases)
- **Lean 4 proofs (`lean/PqCl.lean`):** `4/4` theorems kernel-checked with `0 sorry` (`[propext, Quot.sound]`)
- **Kani 0.68.0 / CBMC 6.11.0 (`cargo kani -Z stubbing --output-format terse`):** `6/6` harnesses `VERIFICATION:- SUCCESSFUL` (`0` failures, `13.4 s` total; abstract zero-allocation hash stubs used for control-flow proofs, unstubbed panic-freedom proof for hex decoding, real SHA-256 in production).

---

## 4. Tri-Signed Hash-Only Custody & Multi-Assumption Frontier

- **Playbox PQAI Root (`playbox`):** Tri-signed `Ed25519 + ML-DSA-65 + SLH-DSA-SHA2-128f` receipts/bundles, `9/9` Lean 4 theorems (`lean/Playbox/Pqai.lean`, `0 sorry`), air-gapped `pqai-audit`/`pqai-migrate` rooms, and pinned offline `SLH-DSA-SHA2-128f` release root (`release.hash.pub`: `89b904d9aeabfeee9a7c00ceeb7f22d026c5d4cf1f50f14812008b98b00dd00e`).
- **Multi-Assumption Cryptanalysis Frontier (`composed-zk-benchmark-os/tools/pqai_frontier/frontier.py`):** Tracks ECC (`ECDSA.fail` verified `secp256k1` circuit `1.82B` Toffoli×Qubits + `BLS12-381`), lattices/codes (`ML-DSA-65`, `Labinius`, `Maverick`), and algebraic vs. conservative hashes (live `Poseidon2` 2-round degree-25 probe vs. `SHA-256`/`BLAKE3` 50% avalanche probes).
