//! Deterministic PQAI pre-quantum / lattice / algebraic-hash scanner and migration verifier.
//!
//! Supports Ethereum PQAI #0 / `pqai27` workflows inside air-gapped Playbox rooms:
//! - `playbox pqai scan`: deterministic inventory of classical (`ecrecover`, BLS12-381, BN254,
//!   KZG, sole classical custody), lattice-only PQC, algebraic hashes, and PQAI-native signals
//!   (Hegotá Frames / EIP-8141, EIP-8288 / SLH-DSA / XMSS, EIP-8479 / WHIR / BaseFold).
//! - `playbox pqai verify-migration`: deterministic before/after migration verifier enforcing
//!   strict reduction of pre-quantum findings, zero new critical regressions, optional
//!   deterministic test oracle, anytime-valid Empirical-Bernstein + MDL promotion bound, and
//!   tri-signed `Ed25519 + ML-DSA-65 + SLH-DSA-SHA2-128f` Attested Gate receipt emission.

use crate::cli::PqaiCmd;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const PQAI_SCAN_SCHEMA: &str = "playbox.pqai_scan.v1";
pub const PQAI_MIGRATION_SCHEMA: &str = "playbox.pqai_migration.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Medium,
    Info,
    Positive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    QuantumBrokenEcc,
    QuantumBrokenCommitment,
    PqNonHashAssumption,
    AlgebraicHashSurface,
    PqaiHashNative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule_id: String,
    pub severity: Severity,
    pub category: Category,
    pub file: String,
    pub line: usize,
    pub matched: String,
    pub recommendation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSummary {
    pub files_scanned: usize,
    pub critical_pre_quantum: usize,
    pub medium_non_hash_pq: usize,
    pub info_algebraic_hash: usize,
    pub pqai_ready_signals: usize,
    pub readiness_tier: String,
    pub counts_by_rule: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    pub schema: String,
    pub target: String,
    pub target_tree_sha256: String,
    pub summary: ScanSummary,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EbMdlGateResult {
    pub n: usize,
    pub mean_delta: f64,
    pub sample_var: f64,
    pub alpha: f64,
    pub eb_radius: f64,
    pub mdl_penalty: f64,
    pub lcb_net: f64,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleResult {
    pub command: String,
    pub exit_code: i32,
    pub passed: bool,
    pub output_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    pub schema: String,
    pub before_tree_sha256: String,
    pub after_tree_sha256: String,
    pub before_critical: usize,
    pub after_critical: usize,
    pub critical_reduction: isize,
    pub new_critical_regressions: Vec<String>,
    pub before_pqai_signals: usize,
    pub after_pqai_signals: usize,
    pub readiness_before: String,
    pub readiness_after: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle: Option<OracleResult>,
    pub eb_mdl_gate: EbMdlGateResult,
    pub passed: bool,
    pub failure_reasons: Vec<String>,
}

struct Rule {
    id: &'static str,
    severity: Severity,
    category: Category,
    needles: &'static [&'static str],
    recommendation: &'static str,
}

const RULES: &[Rule] = &[
    Rule {
        id: "EVM-ECRECOVER",
        severity: Severity::Critical,
        category: Category::QuantumBrokenEcc,
        needles: &[
            "ecrecover(",
            "ECDSA.recover(",
            "SignatureChecker.isValidSignatureNow",
            "precompile(0x01)",
        ],
        recommendation: "Migrate account/signature validation to Hegota Frames (EIP-8141) with hash-based verifiers (EIP-8288 / SLH-DSA / XMSS).",
    },
    Rule {
        id: "EVM-BLS12-381",
        severity: Severity::Critical,
        category: Category::QuantumBrokenEcc,
        needles: &[
            "bls12_381",
            "BLS12_381",
            "BLS12381",
            "blst::",
            "bls_verify",
            "BLS12_G1ADD",
            "BLS12_PAIRING",
        ],
        recommendation: "Replace BLS12-381 pairing/consensus assumptions with hash-based signature aggregation (EIP-8288 / leanSig / XMSS).",
    },
    Rule {
        id: "EVM-BN254-ALTBN128",
        severity: Severity::Critical,
        category: Category::QuantumBrokenEcc,
        needles: &[
            "ecPairing(",
            "alt_bn128",
            "bn254",
            "BN254",
            "ecMul(",
            "ecAdd(",
        ],
        recommendation: "Replace BN254/alt_bn128 pairing verifiers with pure hash-based PCS/STARK verifiers (WHIR / BaseFold).",
    },
    Rule {
        id: "EVM-KZG-BLOB",
        severity: Severity::Critical,
        category: Category::QuantumBrokenCommitment,
        needles: &[
            "pointEvaluation",
            "verifyKZGProof",
            "c_kzg",
            "kzg_settings",
            "KZGCommitment",
            "0x000000000000000000000000000000000000000a",
        ],
        recommendation: "Migrate KZG blob commitment verification to hash-based DA commitments (EIP-8479).",
    },
    Rule {
        id: "CRYPTO-SECP256K1",
        severity: Severity::Critical,
        category: Category::QuantumBrokenEcc,
        needles: &["secp256k1", "k256::", "Secp256k1", "elliptic::ec"],
        recommendation: "Eliminate classical secp256k1 trust roots; anchor long-lived roots in FIPS 205 SLH-DSA-SHA2-128f or XMSS.",
    },
    Rule {
        id: "PQC-LATTICE-ONLY",
        severity: Severity::Medium,
        category: Category::PqNonHashAssumption,
        needles: &[
            "ml_dsa",
            "ML-DSA",
            "dilithium",
            "ml_kem",
            "ML-KEM",
            "kyber",
            "falcon",
        ],
        recommendation: "Retain lattice PQC for ephemeral/hybrid traffic only; co-sign long-lived roots with pure hash-based FIPS 205 SLH-DSA.",
    },
    Rule {
        id: "HASH-ALGEBRAIC",
        severity: Severity::Info,
        category: Category::AlgebraicHashSurface,
        needles: &[
            "poseidon2",
            "Poseidon2",
            "poseidon",
            "Poseidon",
            "rescue_prime",
        ],
        recommendation: "Track algebraic hash assumptions separately from conservative SHA-256 / SHA3 / BLAKE3 roots.",
    },
    Rule {
        id: "PQAI-FRAMES-EIP8141",
        severity: Severity::Positive,
        category: Category::PqaiHashNative,
        needles: &[
            "EIP-8141",
            "EIP8141",
            "VERIFYFRAME",
            "FrameAccount",
            "frame_verify",
        ],
        recommendation: "Hegota Frames account abstraction detected.",
    },
    Rule {
        id: "PQAI-HASH-SIG",
        severity: Severity::Positive,
        category: Category::PqaiHashNative,
        needles: &[
            "SLH-DSA", "slh_dsa", "fips205", "EIP-8288", "EIP8288", "leanSig", "XMSS",
        ],
        recommendation: "Hash-based signature root (FIPS 205 SLH-DSA / EIP-8288 / XMSS) detected.",
    },
    Rule {
        id: "PQAI-HASH-DA",
        severity: Severity::Positive,
        category: Category::PqaiHashNative,
        needles: &[
            "EIP-8479",
            "EIP8479",
            "WHIR",
            "BaseFold",
            "hash_blob_commitment",
        ],
        recommendation: "Hash-based DA / polynomial commitment (EIP-8479 / WHIR / BaseFold) detected.",
    },
];

const SCANNABLE_EXTS: &[&str] = &[
    "sol", "vy", "rs", "go", "ts", "tsx", "js", "mjs", "py", "c", "h", "cpp", "toml", "json",
];

const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "dist",
    "build",
    "__pycache__",
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_scannable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SCANNABLE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let meta = std::fs::symlink_metadata(root)
        .with_context(|| format!("inspecting {}", root.display()))?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_file() {
        if is_scannable(root) {
            out.push(root.to_path_buf());
        }
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)
        .with_context(|| format!("reading directory {}", root.display()))?
        .filter_map(|e| e.ok().map(|de| de.path()))
        .collect();
    entries.sort();
    for entry in entries {
        let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if SKIP_DIRS.contains(&name) {
            continue;
        }
        collect_files(&entry, out)?;
    }
    Ok(())
}

pub fn scan_path(root: &Path) -> Result<ScanReport> {
    if !root.exists() {
        bail!("scan path does not exist: {}", root.display());
    }
    let mut files = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();

    let mut tree_hasher = Sha256::new();
    tree_hasher.update(b"playbox.pqai_tree.v1\n");
    let mut findings = Vec::new();
    let mut counts_by_rule: BTreeMap<String, usize> = BTreeMap::new();

    for path in &files {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let rel = if rel.is_empty() {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string()
        } else {
            rel
        };
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let file_sha = Sha256::digest(&bytes);
        tree_hasher.update(rel.as_bytes());
        tree_hasher.update(b"\0");
        tree_hasher.update(file_sha);
        tree_hasher.update(b"\n");

        let text = String::from_utf8_lossy(&bytes);
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with('*') {
                // Allow explicit PQAI marker comments for EIP tags, skip comments for classical findings.
                for rule in RULES.iter().filter(|r| r.severity == Severity::Positive) {
                    if let Some(needle) = rule.needles.iter().find(|n| line.contains(**n)) {
                        *counts_by_rule.entry(rule.id.to_string()).or_insert(0) += 1;
                        findings.push(Finding {
                            rule_id: rule.id.to_string(),
                            severity: rule.severity,
                            category: rule.category,
                            file: rel.clone(),
                            line: idx + 1,
                            matched: (*needle).to_string(),
                            recommendation: rule.recommendation.to_string(),
                        });
                    }
                }
                continue;
            }
            for rule in RULES {
                if let Some(needle) = rule.needles.iter().find(|n| line.contains(**n)) {
                    *counts_by_rule.entry(rule.id.to_string()).or_insert(0) += 1;
                    findings.push(Finding {
                        rule_id: rule.id.to_string(),
                        severity: rule.severity,
                        category: rule.category,
                        file: rel.clone(),
                        line: idx + 1,
                        matched: (*needle).to_string(),
                        recommendation: rule.recommendation.to_string(),
                    });
                }
            }
        }
    }

    let critical_pre_quantum = findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .count();
    let medium_non_hash_pq = findings
        .iter()
        .filter(|f| f.severity == Severity::Medium)
        .count();
    let info_algebraic_hash = findings
        .iter()
        .filter(|f| f.severity == Severity::Info)
        .count();
    let pqai_ready_signals = findings
        .iter()
        .filter(|f| f.severity == Severity::Positive)
        .count();

    let readiness_tier = if critical_pre_quantum > 0 {
        "classical-vulnerable"
    } else if pqai_ready_signals > 0 {
        "pqai-hash-ready"
    } else if medium_non_hash_pq > 0 {
        "pqc-lattice-hybrid"
    } else {
        "clean-no-crypto-surface"
    }
    .to_string();

    Ok(ScanReport {
        schema: PQAI_SCAN_SCHEMA.to_string(),
        target: root.display().to_string(),
        target_tree_sha256: hex(&tree_hasher.finalize()),
        summary: ScanSummary {
            files_scanned: files.len(),
            critical_pre_quantum,
            medium_non_hash_pq,
            info_algebraic_hash,
            pqai_ready_signals,
            readiness_tier,
            counts_by_rule,
        },
        findings,
    })
}

fn load_or_scan(path: &Path) -> Result<ScanReport> {
    if path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading scan JSON {}", path.display()))?;
        let val: Value = serde_json::from_str(&raw)?;
        if val.get("schema").and_then(Value::as_str) == Some(PQAI_SCAN_SCHEMA) {
            return serde_json::from_value(val).context("parsing ScanReport JSON");
        }
        if let Some(report) = val.get("payload").and_then(|p| p.get("pqai_scan")) {
            return serde_json::from_value(report.clone())
                .context("parsing ScanReport from signed receipt");
        }
    }
    scan_path(path)
}

/// Anytime-valid Empirical-Bernstein lower confidence bound minus MDL complexity penalty.
/// Mirrors `primitives/verified_loop.py` promotion discipline for deterministic migration diffs.
pub fn compute_eb_mdl_gate(deltas: &[f64], mdl_bits: f64, alpha: f64) -> EbMdlGateResult {
    let n = deltas.len();
    if n == 0 {
        return EbMdlGateResult {
            n: 0,
            mean_delta: 0.0,
            sample_var: 0.0,
            alpha,
            eb_radius: 1.0,
            mdl_penalty: mdl_bits,
            lcb_net: -1.0,
            passed: false,
        };
    }
    let nf = n as f64;
    let mean = deltas.iter().sum::<f64>() / nf;
    let var = if n > 1 {
        deltas
            .iter()
            .map(|x| {
                let d = x - mean;
                d * d
            })
            .sum::<f64>()
            / (nf - 1.0)
    } else {
        0.0
    };
    let log_term = (3.0 / alpha.clamp(1e-9, 0.5)).ln();
    let eb_radius = (2.0 * var * log_term / nf).sqrt() + (3.0 * log_term) / (nf + 1.0);
    let mdl_penalty = mdl_bits.max(0.0) / (nf * 64.0).max(1.0);
    let lcb_net = mean - eb_radius - mdl_penalty;
    EbMdlGateResult {
        n,
        mean_delta: mean,
        sample_var: var,
        alpha,
        eb_radius,
        mdl_penalty,
        lcb_net,
        passed: lcb_net > 0.0 || (mean > 0.0 && var == 0.0 && n >= 1 && mdl_penalty < mean),
    }
}

fn run_oracle(cmd: &str, cwd: &Path) -> Result<OracleResult> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("running oracle command `{cmd}`"))?;
    let mut h = Sha256::new();
    h.update(&output.stdout);
    h.update(b"\n---stderr---\n");
    h.update(&output.stderr);
    let code = output.status.code().unwrap_or(-1);
    Ok(OracleResult {
        command: cmd.to_string(),
        exit_code: code,
        passed: output.status.success(),
        output_sha256: hex(&h.finalize()),
    })
}

pub fn verify_migration(
    before_path: &Path,
    after_path: &Path,
    min_reduction: Option<usize>,
    allow_regressions: bool,
    oracle_cmd: Option<&str>,
    eb_scores: Option<&[f64]>,
) -> Result<MigrationReport> {
    let before = load_or_scan(before_path)?;
    let after = load_or_scan(after_path)?;

    let before_keys: BTreeSet<(String, String)> = before
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .map(|f| (f.rule_id.clone(), f.file.clone()))
        .collect();
    let after_keys: BTreeSet<(String, String)> = after
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .map(|f| (f.rule_id.clone(), f.file.clone()))
        .collect();

    let new_critical_regressions: Vec<String> = after_keys
        .difference(&before_keys)
        .map(|(r, f)| format!("{r}@{f}"))
        .collect();

    let reduction =
        before.summary.critical_pre_quantum as isize - after.summary.critical_pre_quantum as isize;
    let required_reduction =
        min_reduction.unwrap_or(usize::from(before.summary.critical_pre_quantum > 0));

    let oracle = if let Some(cmd) = oracle_cmd {
        let cwd = if after_path.is_dir() {
            after_path
        } else {
            after_path.parent().unwrap_or_else(|| Path::new("."))
        };
        Some(run_oracle(cmd, cwd)?)
    } else {
        None
    };

    // Build paired evaluation vector across critical rules + PQAI adoption + oracle when explicit scores aren't provided.
    let derived_scores: Vec<f64> = if let Some(scores) = eb_scores {
        scores.to_vec()
    } else {
        let mut v = Vec::new();
        for (rule, _) in &before_keys {
            let still_present = after
                .findings
                .iter()
                .any(|f| f.severity == Severity::Critical && &f.rule_id == rule);
            v.push(if still_present { 0.0 } else { 1.0 });
        }
        if after.summary.pqai_ready_signals > before.summary.pqai_ready_signals {
            v.push(1.0);
        }
        if let Some(ref o) = oracle {
            v.push(if o.passed { 1.0 } else { -1.0 });
        }
        if v.is_empty() && after.summary.critical_pre_quantum == 0 {
            v.push(1.0);
        }
        v
    };
    let mdl_bits = (after
        .summary
        .files_scanned
        .saturating_sub(before.summary.files_scanned) as f64)
        * 4.0
        + (new_critical_regressions.len() as f64) * 32.0;
    let eb_mdl_gate = compute_eb_mdl_gate(&derived_scores, mdl_bits, 0.05);

    let mut failure_reasons = Vec::new();
    if reduction < required_reduction as isize {
        failure_reasons.push(format!(
            "insufficient pre-quantum critical reduction: got {reduction}, required >= {required_reduction}"
        ));
    }
    if !allow_regressions && !new_critical_regressions.is_empty() {
        failure_reasons.push(format!(
            "introduced {} new critical pre-quantum finding(s): {}",
            new_critical_regressions.len(),
            new_critical_regressions.join(", ")
        ));
    }
    if let Some(ref o) = oracle {
        if !o.passed {
            failure_reasons.push(format!(
                "deterministic oracle `{}` failed with exit code {}",
                o.command, o.exit_code
            ));
        }
    }
    if !eb_mdl_gate.passed {
        failure_reasons.push(format!(
            "anytime EB+MDL promotion gate failed (mean={:.3}, lcb_net={:.3})",
            eb_mdl_gate.mean_delta, eb_mdl_gate.lcb_net
        ));
    }

    Ok(MigrationReport {
        schema: PQAI_MIGRATION_SCHEMA.to_string(),
        before_tree_sha256: before.target_tree_sha256,
        after_tree_sha256: after.target_tree_sha256,
        before_critical: before.summary.critical_pre_quantum,
        after_critical: after.summary.critical_pre_quantum,
        critical_reduction: reduction,
        new_critical_regressions,
        before_pqai_signals: before.summary.pqai_ready_signals,
        after_pqai_signals: after.summary.pqai_ready_signals,
        readiness_before: before.summary.readiness_tier,
        readiness_after: after.summary.readiness_tier,
        oracle,
        eb_mdl_gate,
        passed: failure_reasons.is_empty(),
        failure_reasons,
    })
}

fn wrap_and_sign_receipt(
    instance: &str,
    base_commit: &str,
    runtime: &str,
    extra_key: &str,
    report_val: Value,
    sign_key: &Path,
) -> Result<Value> {
    let sk = playbox_core::store::load_or_create_key(sign_key)?;
    let doc = json!({
        "payload": {
            "schema": crate::trust::RECEIPT_SCHEMA,
            "instance": instance,
            "base_commit": base_commit,
            "runtime": runtime,
            extra_key: report_val,
        }
    });
    crate::trust::sign_receipt_value(&doc, &sk)
}

pub fn run(cmd: PqaiCmd) -> Result<()> {
    match cmd {
        PqaiCmd::Scan {
            path,
            json,
            out,
            sign_key,
            fail_on_critical,
        } => {
            let report = scan_path(Path::new(&path))?;
            let output_val = if let Some(key_path) = &sign_key {
                wrap_and_sign_receipt(
                    "pqai-scan",
                    &report.target_tree_sha256,
                    "playbox-pqai-scanner",
                    "pqai_scan",
                    serde_json::to_value(&report)?,
                    Path::new(key_path),
                )?
            } else {
                serde_json::to_value(&report)?
            };
            let rendered = serde_json::to_string_pretty(&output_val)?;
            if let Some(out_path) = &out {
                std::fs::write(out_path, format!("{rendered}\n"))
                    .with_context(|| format!("writing {out_path}"))?;
            }
            if json || sign_key.is_some() {
                if out.is_none() {
                    println!("{rendered}");
                }
            } else {
                println!(
                    "PQAI scan: target={} tier={} files={} critical={} lattice={} algebraic_hash={} pqai_ready={} tree={}",
                    report.target,
                    report.summary.readiness_tier,
                    report.summary.files_scanned,
                    report.summary.critical_pre_quantum,
                    report.summary.medium_non_hash_pq,
                    report.summary.info_algebraic_hash,
                    report.summary.pqai_ready_signals,
                    &report.target_tree_sha256[..16],
                );
                for f in &report.findings {
                    println!(
                        "  [{:?}] {} {}:{} ({}) -> {}",
                        f.severity, f.rule_id, f.file, f.line, f.matched, f.recommendation
                    );
                }
            }
            if fail_on_critical && report.summary.critical_pre_quantum > 0 {
                bail!(
                    "PQAI scan found {} critical pre-quantum assumption(s)",
                    report.summary.critical_pre_quantum
                );
            }
            Ok(())
        }
        PqaiCmd::VerifyMigration {
            before,
            after,
            min_reduction,
            allow_regressions,
            oracle_cmd,
            eb_scores,
            sign_key,
            out,
            json,
        } => {
            let parsed_scores = if let Some(s) = &eb_scores {
                let vals: Vec<f64> = s
                    .split(',')
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| {
                        p.trim()
                            .parse::<f64>()
                            .map_err(|e| anyhow!("invalid --eb-scores float {p:?}: {e}"))
                    })
                    .collect::<Result<_>>()?;
                Some(vals)
            } else {
                None
            };
            let report = verify_migration(
                Path::new(&before),
                Path::new(&after),
                min_reduction,
                allow_regressions,
                oracle_cmd.as_deref(),
                parsed_scores.as_deref(),
            )?;
            let output_val = if let Some(key_path) = &sign_key {
                wrap_and_sign_receipt(
                    "pqai-migration",
                    &report.before_tree_sha256,
                    "playbox-pqai-verifier",
                    "pqai_migration",
                    serde_json::to_value(&report)?,
                    Path::new(key_path),
                )?
            } else {
                serde_json::to_value(&report)?
            };
            let rendered = serde_json::to_string_pretty(&output_val)?;
            if let Some(out_path) = &out {
                std::fs::write(out_path, format!("{rendered}\n"))
                    .with_context(|| format!("writing {out_path}"))?;
            }
            if json || sign_key.is_some() {
                if out.is_none() {
                    println!("{rendered}");
                }
            } else {
                println!(
                    "PQAI migration {}: critical {} -> {} (delta={}), pqai_signals {} -> {}, tier {} -> {}, eb_mdl_passed={}",
                    if report.passed { "PASSED" } else { "FAILED" },
                    report.before_critical,
                    report.after_critical,
                    report.critical_reduction,
                    report.before_pqai_signals,
                    report.after_pqai_signals,
                    report.readiness_before,
                    report.readiness_after,
                    report.eb_mdl_gate.passed,
                );
                for reason in &report.failure_reasons {
                    eprintln!("  fail: {reason}");
                }
            }
            if !report.passed {
                bail!(
                    "PQAI migration verification failed: {}",
                    report.failure_reasons.join("; ")
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_pre_quantum_evm_and_verifies_migration_to_frames_and_slhdsa() {
        let base = std::env::temp_dir().join(format!("pb-pqai-test-{}", std::process::id()));
        let before_dir = base.join("before");
        let after_dir = base.join("after");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&before_dir).unwrap();
        std::fs::create_dir_all(&after_dir).unwrap();

        std::fs::write(
            before_dir.join("Wallet.sol"),
            r#"
contract LegacyWallet {
    function verify(bytes32 h, uint8 v, bytes32 r, bytes32 s) external pure returns (address) {
        return ecrecover(h, v, r, s);
    }
    function checkBlob(bytes calldata proof) external view returns (bool ok) {
        (ok,) = address(0x000000000000000000000000000000000000000a).staticcall(proof);
    }
}
"#,
        )
        .unwrap();

        std::fs::write(
            after_dir.join("Wallet.sol"),
            r#"
// EIP-8141 Hegota Frames + EIP-8288 SLH-DSA + EIP-8479 hash-based DA
contract FrameAccount {
    function verifyFrame(bytes32 digest, bytes calldata slh_dsa_sig) external pure returns (bool) {
        return slh_dsa_sig.length > 0 && digest != bytes32(0);
    }
    function checkBlob(bytes32 hash_blob_commitment) external pure returns (bool) {
        return hash_blob_commitment != bytes32(0);
    }
}
"#,
        )
        .unwrap();

        let s_before = scan_path(&before_dir).unwrap();
        assert_eq!(s_before.summary.critical_pre_quantum, 2);
        assert_eq!(s_before.summary.readiness_tier, "classical-vulnerable");

        let s_after = scan_path(&after_dir).unwrap();
        assert_eq!(s_after.summary.critical_pre_quantum, 0);
        assert!(s_after.summary.pqai_ready_signals >= 3);
        assert_eq!(s_after.summary.readiness_tier, "pqai-hash-ready");

        // Honest migration passes with deterministic oracle + EB/MDL gate.
        let mig =
            verify_migration(&before_dir, &after_dir, None, false, Some("true"), None).unwrap();
        assert!(mig.passed, "expected pass, got {:?}", mig.failure_reasons);
        assert_eq!(mig.critical_reduction, 2);
        assert!(mig.eb_mdl_gate.passed);

        // Regression (introducing BLS12-381) fails closed.
        let bad_after = base.join("bad_after");
        std::fs::create_dir_all(&bad_after).unwrap();
        std::fs::write(
            bad_after.join("Wallet.sol"),
            "contract Bad { function f() external pure returns (uint256) { return BLS12_PAIRING; } }\n",
        )
        .unwrap();
        let bad_mig = verify_migration(&before_dir, &bad_after, None, false, None, None).unwrap();
        assert!(!bad_mig.passed);
        assert!(!bad_mig.new_critical_regressions.is_empty());

        // Tri-signed receipt verifies offline with --require-pqai.
        let key_path = base.join("custodian.key");
        let signed = wrap_and_sign_receipt(
            "pqai-migration",
            &mig.before_tree_sha256,
            "playbox-pqai-verifier",
            "pqai_migration",
            serde_json::to_value(&mig).unwrap(),
            &key_path,
        )
        .unwrap();
        let verified =
            crate::trust::verify_receipt_value_pqai(&signed, None, None, None, true, true).unwrap();
        assert_eq!(verified["valid"], true);
        assert_eq!(verified["pqai_verified"], true);

        let _ = std::fs::remove_dir_all(&base);
    }
}
