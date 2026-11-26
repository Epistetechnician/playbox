# PQAI End-to-End Verification & ETH R&D Evidence Bundle (v0.4.7)

Reproducible local verification summary across all five layers aligned with Ethereum's [PQAI #0](https://blog.ethereum.org/2026/11/23/pqai-0) and Ethlabs' `pqai27` hash-only minimal-assumption roadmap.

## Claim Boundary
- **Scope:** Local engineering, formal verification, differential testing, adversarial mutation, and Apple Silicon ARM64 benchmark measurements.
- **Not claimed:** AWS Graviton4 / Raspberry Pi 5 measurements (script packaged, not yet run on remote hosts), external audits, or mainnet/production deployment.

---

## Layer 1 — Pure Hash-Based PQAI Trust Root (`playbox`)
- **Primitives (`crates/playbox-core/src/pq.rs`):** Tri-signed custody (`Ed25519 + FIPS 204 ML-DSA-65 + FIPS 205 SLH-DSA-SHA2-128f`), anchoring long-lived roots (`RoomLog` epoch checkpoints, `BiscuitRootAnchor`, Attested Gate receipts, 5-gate bundles, releases) under pure hash-based signatures.
- **Formal proofs (`lean/Playbox/Pqai.lean`):** Kernel-checked Lean 4 theorems (`0 sorry`, checked by `lake build`) proving:
  - `anchor_cross_room_denies`, `anchor_unpinned_slh_denies`, `anchor_invalid_sig_denies`, `anchor_revoked_denies`, `anchor_revocation_monotonic`.
  - `pqai_requires_hash_sig`, `pqai_implies_hybrid`, `pqai_count_le_hybrid_count`, `pqai_checkpoint_safety` (strict majority quorum intersection `2k > n`).

## Layer 2 — EF `pq-cl`, EIP-8288 & EIP-8479 Proving Ground

### 2A. Home-Staker ARM64 Ceilings (Apple M4 Max, 1 thread)
Full raw artifacts live in `docs/reports/pqai-arm-bench-m4max-v0.4.7/`.
- **Signatures (`8,192` attestations / `4s` slot):**
  - `Ed25519`: `64 B`, `0.018 ms` verify (`1.0 Mbps`, raw feasible).
  - `ML-DSA-65`: `3,309 B`, `0.084 ms` verify (`54.2 Mbps`, needs aggregation).
  - `XMSS-WOTS+-SHA256` (`H=10`, leanSig-style): `2,468 B`, `0.116 ms` verify (`40.4 Mbps`, needs EIP-8288 aggregation).
  - `SLH-DSA-SHA2-128f`: `17,088 B`, `1.457 ms` verify (`280.0 Mbps`, roots/checkpoints only).
- **Hash-based PCS vs. Lattice PCS (`2^18` witness, suite `s`, 1 thread):**
  - `binius64 BaseFold` (1/4, 100b): `0.24 ms` verify, `188.8 KB` proof.
  - `binius64 WHIR` (1/4, 100b): `0.50 ms` verify, `158.1 KB` proof.
  - `flock-core Ligerito Slim100` (1/4, 100b): `0.32 ms` verify, `180.8 KB` proof.
  - `Labinius` (lattice baseline): `4.15–7.80 ms` verify, `591–610 KB` wire.
- **Multi-ARM runner:** `./scripts/run_pqai_arm_bench.sh` (ready for Graviton4 `c8g` and Raspberry Pi 5).

### 2B. EIP-8288 & EIP-8479 Soundness-Before-Speed Matrix (`composed-zk-benchmark-os`)
- `crates/zkbench-core/src/pqai.rs` + `examples/pqai_soundness_matrix.rs`: **14 adversarial mutation classes × 2 protocols = 28 attack cases**.
- Fail-closed reference verifier: **ADMITTED (`28/28` caught, `0` unsound accepts)**, `31.3 us` (EIP-8288) / `83.0 us` (EIP-8479).
- Unsound fast baseline: **DISQUALIFIED (`2/28` caught, `26` unsound accepts)** with timings quarantined.

### 2C. Triple-Lock `pq-cl` Verifier (`formasec`)
- **Executable Python spec:** `search/pqcl_spec.py`
- **Portable Rust verifier + Kani harnesses:** `pqcl-verifier/src/lib.rs`
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

# Layer 2C (formasec)
cd formasec/pqcl-verifier && cargo test
cd .. && lean lean/PqCl.lean && python3 search/tests/test_pqcl_differential.py -v

# Layer 5 HyperIntel & hyperAI
cargo test --manifest-path HyperIntel/Cargo.toml -p hyperintel-assurance
cargo test --manifest-path hyperAI/Cargo.toml -p hyperai-scm-adapter
```
