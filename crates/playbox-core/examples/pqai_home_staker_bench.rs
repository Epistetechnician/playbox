//! PQAI Home-Staker Signature & PCS Benchmark Harness (Layer 2A).
//!
//! Benchmarks real signature primitives on the local CPU (Ed25519, FIPS 204 ML-DSA-65,
//! FIPS 205 SLH-DSA-SHA2-128f, and stateful XMSS-WOTS+-SHA256 modeled after EF leanSig /
//! EIP-8288 validator signatures), measures wall-clock latency, signature/public-key sizes,
//! and peak RSS, evaluates Ethereum home-staker ceilings (4s attestation window, 12s slot,
//! 25/100 Mbps bandwidth, 16 GiB RAM), and optionally ingests live `pcs-competitors` /
//! `labinius` PCS tables (WHIR, BaseFold, Brakedown, Ligerito, Labinius).

use std::fs;
use std::time::Instant;

use ed25519_dalek::{Signer, SigningKey, Verifier};
use playbox_core::pq::{
    keygen_mldsa65, keygen_slhdsa, sign_mldsa65, sign_slhdsa, verify_mldsa65, verify_slhdsa,
};
use serde_json::json;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Minimal real XMSS-WOTS+-SHA256 (H=10 => 1,024 slots, w=16, n=32 bytes)
// Mirrors the hash-only stateful signature family (leanSig / XMSS) proposed for
// Ethereum pq-cl / EIP-8288 validator attestations.
// ---------------------------------------------------------------------------

const WOTS_N: usize = 32;
const WOTS_W: usize = 16; // log2(w) = 4 bits per nibble
const WOTS_LEN1: usize = 64; // 256 / 4
const WOTS_LEN2: usize = 3; // ceil(log_16(64 * 15) + 1) = 3
const WOTS_LEN: usize = WOTS_LEN1 + WOTS_LEN2; // 67 chains * 32B = 2,144B

fn hash_tag(tag: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(tag);
    for p in parts {
        h.update(p);
    }
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

fn wots_chain(
    mut x: [u8; 32],
    start: u8,
    steps: u8,
    pub_seed: &[u8; 32],
    leaf: u32,
    chain: u8,
) -> [u8; 32] {
    let leaf_b = leaf.to_le_bytes();
    for s in start..(start + steps) {
        x = hash_tag(b"xmss.wots.f.v1", &[pub_seed, &leaf_b, &[chain, s], &x]);
    }
    x
}

fn wots_msg_base_w(msg_digest: &[u8; 32]) -> [u8; WOTS_LEN] {
    let mut digits = [0u8; WOTS_LEN];
    let mut csum: u32 = 0;
    for i in 0..32 {
        let hi = msg_digest[i] >> 4;
        let lo = msg_digest[i] & 0x0f;
        digits[2 * i] = hi;
        digits[2 * i + 1] = lo;
        csum += (WOTS_W as u32 - 1) - hi as u32;
        csum += (WOTS_W as u32 - 1) - lo as u32;
    }
    // 12-bit checksum encoded in 3 base-16 digits (big-endian)
    digits[WOTS_LEN1] = ((csum >> 8) & 0x0f) as u8;
    digits[WOTS_LEN1 + 1] = ((csum >> 4) & 0x0f) as u8;
    digits[WOTS_LEN1 + 2] = (csum & 0x0f) as u8;
    digits
}

fn wots_sk_elem(sk_seed: &[u8; 32], leaf: u32, chain: u8) -> [u8; 32] {
    hash_tag(
        b"xmss.wots.sk.v1",
        &[sk_seed, &leaf.to_le_bytes(), &[chain]],
    )
}

fn wots_pk_leaf(sk_seed: &[u8; 32], pub_seed: &[u8; 32], leaf: u32) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"xmss.wots.pk.v1");
    h.update(pub_seed);
    h.update(leaf.to_le_bytes());
    for c in 0..WOTS_LEN {
        let sk_i = wots_sk_elem(sk_seed, leaf, c as u8);
        let pk_i = wots_chain(sk_i, 0, (WOTS_W - 1) as u8, pub_seed, leaf, c as u8);
        h.update(pk_i);
    }
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

struct XmssKeypair {
    height: usize,
    sk_seed: [u8; 32],
    pub_seed: [u8; 32],
    root: [u8; 32],
    /// Complete binary Merkle tree layers (layers[0] = leaves, layers[height] = [root]).
    layers: Vec<Vec<[u8; 32]>>,
}

struct XmssSignature {
    leaf_idx: u32,
    wots_sig: [[u8; 32]; WOTS_LEN],
    auth_path: Vec<[u8; 32]>,
}

impl XmssSignature {
    fn byte_len(&self) -> usize {
        4 + WOTS_LEN * WOTS_N + self.auth_path.len() * 32
    }
}

impl XmssKeypair {
    fn keygen(height: usize, sk_seed: [u8; 32], pub_seed: [u8; 32]) -> Self {
        let n_leaves = 1usize << height;
        let mut leaves = Vec::with_capacity(n_leaves);
        for i in 0..n_leaves {
            leaves.push(wots_pk_leaf(&sk_seed, &pub_seed, i as u32));
        }
        let mut layers = vec![leaves];
        for lvl in 0..height {
            let prev = &layers[lvl];
            let mut next = Vec::with_capacity(prev.len() / 2);
            for idx in 0..(prev.len() / 2) {
                let node = hash_tag(
                    b"xmss.merkle.node.v1",
                    &[
                        &pub_seed,
                        &[lvl as u8],
                        &(idx as u32).to_le_bytes(),
                        &prev[2 * idx],
                        &prev[2 * idx + 1],
                    ],
                );
                next.push(node);
            }
            layers.push(next);
        }
        let root = layers[height][0];
        Self {
            height,
            sk_seed,
            pub_seed,
            root,
            layers,
        }
    }

    fn sign(&self, leaf_idx: u32, msg: &[u8]) -> XmssSignature {
        let msg_digest = hash_tag(
            b"xmss.msg.v1",
            &[&self.pub_seed, &self.root, &leaf_idx.to_le_bytes(), msg],
        );
        let digits = wots_msg_base_w(&msg_digest);
        let mut wots_sig = [[0u8; 32]; WOTS_LEN];
        for c in 0..WOTS_LEN {
            let sk_i = wots_sk_elem(&self.sk_seed, leaf_idx, c as u8);
            wots_sig[c] = wots_chain(sk_i, 0, digits[c], &self.pub_seed, leaf_idx, c as u8);
        }
        let mut auth_path = Vec::with_capacity(self.height);
        let mut idx = leaf_idx as usize;
        for lvl in 0..self.height {
            auth_path.push(self.layers[lvl][idx ^ 1]);
            idx >>= 1;
        }
        XmssSignature {
            leaf_idx,
            wots_sig,
            auth_path,
        }
    }

    fn verify(root: &[u8; 32], pub_seed: &[u8; 32], msg: &[u8], sig: &XmssSignature) -> bool {
        let msg_digest = hash_tag(
            b"xmss.msg.v1",
            &[pub_seed, root, &sig.leaf_idx.to_le_bytes(), msg],
        );
        let digits = wots_msg_base_w(&msg_digest);
        let mut h = Sha256::new();
        h.update(b"xmss.wots.pk.v1");
        h.update(pub_seed);
        h.update(sig.leaf_idx.to_le_bytes());
        for (c, (&digit, &sig_elem)) in digits.iter().zip(sig.wots_sig.iter()).enumerate() {
            let rem = (WOTS_W as u8 - 1) - digit;
            let pk_i = wots_chain(sig_elem, digit, rem, pub_seed, sig.leaf_idx, c as u8);
            h.update(pk_i);
        }
        let out = h.finalize();
        let mut node = [0u8; 32];
        node.copy_from_slice(&out);
        let mut idx = sig.leaf_idx as usize;
        for (lvl, sib) in sig.auth_path.iter().enumerate() {
            let (left, right) = if idx & 1 == 0 {
                (&node, sib)
            } else {
                (sib, &node)
            };
            let parent_idx = (idx >> 1) as u32;
            node = hash_tag(
                b"xmss.merkle.node.v1",
                &[
                    pub_seed,
                    &[lvl as u8],
                    &parent_idx.to_le_bytes(),
                    left,
                    right,
                ],
            );
            idx >>= 1;
        }
        node == *root
    }
}

// ---------------------------------------------------------------------------
// Reporting types & Home-Staker Ceiling Evaluation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SigBenchRow {
    scheme: String,
    assumption_family: String,
    pqai_hash_only: bool,
    pk_bytes: usize,
    sig_bytes: usize,
    keygen_ms: f64,
    sign_ms: f64,
    verify_ms: f64,
    verify_ops_per_sec_1core: f64,
    /// Raw bandwidth (Mbps) required if 8,192 attestations/slot are broadcast unaggregated in a 4s window.
    unagg_8192_att_4s_mbps: f64,
    /// Single-core time (ms) to verify 8,192 attestations unaggregated.
    unagg_8192_verify_1core_ms: f64,
    /// Whether raw unaggregated 8,192 attestations fit inside home-staker 25 Mbps & 4s/1-core budgets.
    raw_fits_home_staker_25mbps_4s: bool,
}

#[derive(Debug, Clone)]
struct PcsBenchRow {
    scheme: String,
    assumption_family: String,
    pqai_hash_only: bool,
    rate: String,
    target_bits: String,
    commit_ms: f64,
    open_ms: f64,
    verify_ms: f64,
    commitment_bytes: usize,
    proof_bytes: usize,
    fits_home_staker_verify_100ms: bool,
    fits_eip8479_128kb_proof: bool,
}

#[derive(Debug, Clone)]
struct HomeStakerCeilings {
    attestation_window_secs: f64,
    slot_duration_secs: f64,
    bandwidth_budget_mbps: f64,
    ram_budget_gib: f64,
    committee_size_per_slot: usize,
    target_pcs_verify_ms: f64,
    target_proof_size_bytes: usize,
}

#[derive(Debug, Clone)]
struct HomeStakerReport {
    schema_version: u32,
    claim_ceiling: String,
    hardware_note: String,
    host_arch: String,
    host_os: String,
    peak_rss_mib: f64,
    ceilings: HomeStakerCeilings,
    signatures: Vec<SigBenchRow>,
    pcs: Vec<PcsBenchRow>,
}

fn median(vals: &mut [f64]) -> f64 {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = vals.len();
    if n % 2 == 1 {
        vals[n / 2]
    } else {
        0.5 * (vals[n / 2 - 1] + vals[n / 2])
    }
}

fn parse_pcs_competitors_output(raw: &str) -> Vec<PcsBenchRow> {
    let mut rows = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        // Table rows look like:
        // BaseFold (binius64) 1/2 100 123.45 45.67 1.23 32 145000
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 8 {
            continue;
        }
        // Last 5 tokens must parse as f64, f64, f64, usize, usize
        let n = parts.len();
        let (Ok(commit_ms), Ok(open_ms), Ok(verify_ms), Ok(commitment_bytes), Ok(proof_bytes)) = (
            parts[n - 5].parse::<f64>(),
            parts[n - 4].parse::<f64>(),
            parts[n - 3].parse::<f64>(),
            parts[n - 2].parse::<usize>(),
            parts[n - 1].parse::<usize>(),
        ) else {
            continue;
        };
        let target_bits = parts[n - 6].to_string();
        let rate = parts[n - 7].to_string();
        let scheme = parts[..n - 7].join(" ");
        let is_lattice = scheme.to_lowercase().contains("labinius");
        rows.push(PcsBenchRow {
            scheme,
            assumption_family: if is_lattice {
                "cyclotomic-ring-lattice (Module-SIS)".into()
            } else {
                "hash-only (Merkle + Reed-Solomon / tensor codes)".into()
            },
            pqai_hash_only: !is_lattice,
            rate,
            target_bits,
            commit_ms,
            open_ms,
            verify_ms,
            commitment_bytes,
            proof_bytes,
            fits_home_staker_verify_100ms: verify_ms <= 100.0,
            fits_eip8479_128kb_proof: proof_bytes <= 131_072,
        });
    }
    rows
}

fn peak_rss_mib() -> f64 {
    // Portable best-effort RSS read via /usr/bin/ps on Unix (avoids adding libc dependency).
    let pid = std::process::id().to_string();
    if let Ok(out) = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
    {
        if let Ok(s) = String::from_utf8(out.stdout) {
            if let Ok(kb) = s.trim().parse::<f64>() {
                return kb / 1024.0;
            }
        }
    }
    0.0
}

#[allow(clippy::too_many_arguments)]
fn make_sig_row(
    scheme: &str,
    assumption_family: &str,
    pqai_hash_only: bool,
    pk_bytes: usize,
    sig_bytes: usize,
    keygen_ms: f64,
    sign_ms: f64,
    verify_ms: f64,
) -> SigBenchRow {
    let verify_ops_per_sec_1core = if verify_ms > 0.0 {
        1000.0 / verify_ms
    } else {
        0.0
    };
    let committee = 8192.0;
    let window_secs = 4.0;
    let unagg_mbps = (committee * sig_bytes as f64 * 8.0) / (window_secs * 1_000_000.0);
    let unagg_verify_ms = committee * verify_ms;
    SigBenchRow {
        scheme: scheme.into(),
        assumption_family: assumption_family.into(),
        pqai_hash_only,
        pk_bytes,
        sig_bytes,
        keygen_ms,
        sign_ms,
        verify_ms,
        verify_ops_per_sec_1core,
        unagg_8192_att_4s_mbps: unagg_mbps,
        unagg_8192_verify_1core_ms: unagg_verify_ms,
        raw_fits_home_staker_25mbps_4s: unagg_mbps <= 25.0 && unagg_verify_ms <= 4000.0,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut pcs_file: Option<String> = None;
    let mut json_out: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--pcs-file" if i + 1 < args.len() => {
                pcs_file = Some(args[i + 1].clone());
                i += 2;
            }
            "--json-out" if i + 1 < args.len() => {
                json_out = Some(args[i + 1].clone());
                i += 2;
            }
            _ => i += 1,
        }
    }

    let msg = b"ethereum.pq-cl.eip8288.attestation.v1|slot=1048576|epoch=32768|block=0xdeadbeef";
    let mut sig_rows = Vec::new();

    // 1. Ed25519 (pre-quantum baseline)
    {
        let mut kg = Vec::new();
        let mut sg = Vec::new();
        let mut vf = Vec::new();
        let sk = SigningKey::from_bytes(&[0x42u8; 32]);
        let pk = sk.verifying_key();
        let sig = sk.sign(msg);
        for _ in 0..25 {
            let t0 = Instant::now();
            let k = SigningKey::from_bytes(&[0x42u8; 32]);
            let _ = k.verifying_key();
            kg.push(t0.elapsed().as_secs_f64() * 1000.0);

            let t1 = Instant::now();
            let s = sk.sign(msg);
            sg.push(t1.elapsed().as_secs_f64() * 1000.0);

            let t2 = Instant::now();
            assert!(pk.verify(msg, &s).is_ok());
            vf.push(t2.elapsed().as_secs_f64() * 1000.0);
        }
        sig_rows.push(make_sig_row(
            "Ed25519 (classical)",
            "ECC (Curve25519 — broken by Shor)",
            false,
            pk.to_bytes().len(),
            sig.to_bytes().len(),
            median(&mut kg),
            median(&mut sg),
            median(&mut vf),
        ));
    }

    // 2. ML-DSA-65 (FIPS 204 — lattice PQC)
    {
        let mut kg = Vec::new();
        let mut sg = Vec::new();
        let mut vf = Vec::new();
        let ctx = b"ethereum.pq-cl.eip8288.v1";
        let (pk, sk) = keygen_mldsa65().expect("mldsa65 keygen");
        let sig = sign_mldsa65(&sk, msg, ctx).expect("mldsa65 sign");
        for _ in 0..15 {
            let t0 = Instant::now();
            let _ = keygen_mldsa65().expect("mldsa65 keygen");
            kg.push(t0.elapsed().as_secs_f64() * 1000.0);

            let t1 = Instant::now();
            let s = sign_mldsa65(&sk, msg, ctx).expect("mldsa65 sign");
            sg.push(t1.elapsed().as_secs_f64() * 1000.0);

            let t2 = Instant::now();
            assert!(verify_mldsa65(&pk, msg, &s, ctx));
            vf.push(t2.elapsed().as_secs_f64() * 1000.0);
        }
        sig_rows.push(make_sig_row(
            "ML-DSA-65 (FIPS 204)",
            "Module-LWE / Module-SIS lattice",
            false,
            pk.len(),
            sig.len(),
            median(&mut kg),
            median(&mut sg),
            median(&mut vf),
        ));
    }

    // 3. SLH-DSA-SHA2-128f (FIPS 205 — stateless hash-only PQAI root)
    {
        let mut kg = Vec::new();
        let mut sg = Vec::new();
        let mut vf = Vec::new();
        let ctx = b"ethereum.pq-cl.eip8288.v1";
        let (pk, sk) = keygen_slhdsa().expect("slhdsa keygen");
        let sig = sign_slhdsa(&sk, msg, ctx).expect("slhdsa sign");
        for _ in 0..5 {
            let t0 = Instant::now();
            let _ = keygen_slhdsa().expect("slhdsa keygen");
            kg.push(t0.elapsed().as_secs_f64() * 1000.0);

            let t1 = Instant::now();
            let s = sign_slhdsa(&sk, msg, ctx).expect("slhdsa sign");
            sg.push(t1.elapsed().as_secs_f64() * 1000.0);

            let t2 = Instant::now();
            assert!(verify_slhdsa(&pk, msg, &s, ctx));
            vf.push(t2.elapsed().as_secs_f64() * 1000.0);
        }
        sig_rows.push(make_sig_row(
            "SLH-DSA-SHA2-128f (FIPS 205)",
            "Stateless hash-only (SHA-256)",
            true,
            pk.len(),
            sig.len(),
            median(&mut kg),
            median(&mut sg),
            median(&mut vf),
        ));
    }

    // 4. XMSS-WOTS+-SHA256 H=10 (stateful hash-only consensus signature, leanSig/EIP-8288 model)
    {
        let mut kg = Vec::new();
        let mut sg = Vec::new();
        let mut vf = Vec::new();
        let sk_seed = [0x11u8; 32];
        let pub_seed = [0x22u8; 32];
        let t0 = Instant::now();
        let kp = XmssKeypair::keygen(10, sk_seed, pub_seed);
        kg.push(t0.elapsed().as_secs_f64() * 1000.0);
        let sig0 = kp.sign(0, msg);
        assert!(XmssKeypair::verify(&kp.root, &kp.pub_seed, msg, &sig0));
        for leaf in 1..25u32 {
            let t1 = Instant::now();
            let s = kp.sign(leaf, msg);
            sg.push(t1.elapsed().as_secs_f64() * 1000.0);

            let t2 = Instant::now();
            assert!(XmssKeypair::verify(&kp.root, &kp.pub_seed, msg, &s));
            vf.push(t2.elapsed().as_secs_f64() * 1000.0);
        }
        sig_rows.push(make_sig_row(
            "XMSS-WOTS+-SHA256 (H=10, leanSig-style)",
            "Stateful hash-only (SHA-256 + Merkle)",
            true,
            64, // 32B root + 32B pub_seed
            sig0.byte_len(),
            median(&mut kg),
            median(&mut sg),
            median(&mut vf),
        ));
    }

    let pcs_rows = pcs_file
        .as_deref()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|raw| parse_pcs_competitors_output(&raw))
        .unwrap_or_default();

    let report = HomeStakerReport {
        schema_version: 1,
        claim_ceiling: "LocalDevelopmentAppleSiliconEvidence (not official Graviton4 / Xeon / Pi5)".into(),
        hardware_note: "Measured locally on Apple Silicon host; Graviton4 (SVE2) and Raspberry Pi 5 numbers require native hardware runs.".into(),
        host_arch: std::env::consts::ARCH.into(),
        host_os: std::env::consts::OS.into(),
        peak_rss_mib: peak_rss_mib(),
        ceilings: HomeStakerCeilings {
            attestation_window_secs: 4.0,
            slot_duration_secs: 12.0,
            bandwidth_budget_mbps: 25.0,
            ram_budget_gib: 16.0,
            committee_size_per_slot: 8192,
            target_pcs_verify_ms: 100.0,
            target_proof_size_bytes: 131_072,
        },
        signatures: sig_rows,
        pcs: pcs_rows,
    };

    println!(
        "=== PQAI Home-Staker Signature Ceilings ({}/{}) ===",
        report.host_arch, report.host_os
    );
    println!(
        "{:<40} {:>6} {:>7} {:>9} {:>9} {:>9} {:>10} {:>10} {:>8}",
        "scheme", "pk B", "sig B", "kg ms", "sign ms", "ver ms", "8k Mbps", "8k ver ms", "raw OK?"
    );
    for r in &report.signatures {
        println!(
            "{:<40} {:>6} {:>7} {:>9.2} {:>9.3} {:>9.3} {:>10.1} {:>10.1} {:>8}",
            r.scheme,
            r.pk_bytes,
            r.sig_bytes,
            r.keygen_ms,
            r.sign_ms,
            r.verify_ms,
            r.unagg_8192_att_4s_mbps,
            r.unagg_8192_verify_1core_ms,
            if r.raw_fits_home_staker_25mbps_4s {
                "YES"
            } else {
                "NO (agg)"
            }
        );
    }

    if !report.pcs.is_empty() {
        println!("\n=== PQAI Home-Staker PCS Ceilings (Hash-based vs Lattice) ===");
        println!(
            "{:<30} {:>6} {:>6} {:>10} {:>10} {:>10} {:>9} {:>10} {:>8}",
            "scheme", "rate", "bits", "commit ms", "open ms", "ver ms", "C B", "|pi| B", "PQAI?"
        );
        for r in &report.pcs {
            println!(
                "{:<30} {:>6} {:>6} {:>10.2} {:>10.2} {:>10.2} {:>9} {:>10} {:>8}",
                r.scheme,
                r.rate,
                r.target_bits,
                r.commit_ms,
                r.open_ms,
                r.verify_ms,
                r.commitment_bytes,
                r.proof_bytes,
                if r.pqai_hash_only { "HASH" } else { "LATTICE" }
            );
        }
    }

    if let Some(path) = json_out {
        let sigs_json: Vec<_> = report
            .signatures
            .iter()
            .map(|r| {
                json!({
                    "scheme": r.scheme,
                    "assumption_family": r.assumption_family,
                    "pqai_hash_only": r.pqai_hash_only,
                    "pk_bytes": r.pk_bytes,
                    "sig_bytes": r.sig_bytes,
                    "keygen_ms": r.keygen_ms,
                    "sign_ms": r.sign_ms,
                    "verify_ms": r.verify_ms,
                    "verify_ops_per_sec_1core": r.verify_ops_per_sec_1core,
                    "unagg_8192_att_4s_mbps": r.unagg_8192_att_4s_mbps,
                    "unagg_8192_verify_1core_ms": r.unagg_8192_verify_1core_ms,
                    "raw_fits_home_staker_25mbps_4s": r.raw_fits_home_staker_25mbps_4s,
                })
            })
            .collect();
        let pcs_json: Vec<_> = report
            .pcs
            .iter()
            .map(|r| {
                json!({
                    "scheme": r.scheme,
                    "assumption_family": r.assumption_family,
                    "pqai_hash_only": r.pqai_hash_only,
                    "rate": r.rate,
                    "target_bits": r.target_bits,
                    "commit_ms": r.commit_ms,
                    "open_ms": r.open_ms,
                    "verify_ms": r.verify_ms,
                    "commitment_bytes": r.commitment_bytes,
                    "proof_bytes": r.proof_bytes,
                    "fits_home_staker_verify_100ms": r.fits_home_staker_verify_100ms,
                    "fits_eip8479_128kb_proof": r.fits_eip8479_128kb_proof,
                })
            })
            .collect();
        let payload = json!({
            "schema_version": report.schema_version,
            "claim_ceiling": report.claim_ceiling,
            "hardware_note": report.hardware_note,
            "host_arch": report.host_arch,
            "host_os": report.host_os,
            "peak_rss_mib": report.peak_rss_mib,
            "ceilings": {
                "attestation_window_secs": report.ceilings.attestation_window_secs,
                "slot_duration_secs": report.ceilings.slot_duration_secs,
                "bandwidth_budget_mbps": report.ceilings.bandwidth_budget_mbps,
                "ram_budget_gib": report.ceilings.ram_budget_gib,
                "committee_size_per_slot": report.ceilings.committee_size_per_slot,
                "target_pcs_verify_ms": report.ceilings.target_pcs_verify_ms,
                "target_proof_size_bytes": report.ceilings.target_proof_size_bytes,
            },
            "signatures": sigs_json,
            "pcs": pcs_json,
        });
        let json = serde_json::to_string_pretty(&payload).expect("serialize report");
        fs::write(&path, json).expect("write report json");
        println!("\nWrote JSON report -> {path}");
    }
}
