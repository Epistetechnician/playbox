//! `playbox gate`: export and offline-verify v0.4 Attested Gate compliance receipt bundles.
//!
//! Strictly scoped to the 5 runtime-native Playbox gates (`docs/v0.4-strategy.md`):
//! 1. Capability & grant bounds (`GRANT-1..4`, `CAP-1..8`)
//! 2. MicroVM & Linux confinement (`BOX-2..8`, including `BOX-7` egress logs)
//! 3. Tamper-evident hash-chained logs & hybrid PQ receipts (`ROOM-9..17`, `audit.log`, `RoomLog`)
//! 4. Sealed anti-Goodhart evaluation gates (`RoomLog` promotion quorum + optional challenge receipts)
//! 5. Hardware-bound identity & release integrity (`IDENT-1..8`, `REL-1..9`, `T0..T3` attestation)

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use playbox_core::{
    attest::{self, PlatformVerifier, Tier},
    platform, pq,
    promotion::{Promotion, Receipt as PromoReceipt, Register, Rule},
    record::{hex, unhex},
    roomlog::{self, Config, RoomLog},
    store,
};
use playbox_runner::egress::EgressLogEntry;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

pub const BUNDLE_SCHEMA: &str = "playbox.attested_gate.v1";
const BODY_DOMAIN: &[u8] = b"playbox.attested_gate.v1.body\n";
const SIG_DOMAIN: &[u8] = b"playbox.attested_gate.v1.sig\n";
const PQ_CTX: &[u8] = b"playbox.attested_gate.pq.v1";

pub struct ExportOpts {
    pub state: Option<String>,
    pub audit: Option<String>,
    pub room_log: Option<String>,
    pub root: Option<String>,
    pub egress_log: Option<String>,
    pub challenge_log: Option<String>,
    pub receipts: Vec<String>,
    pub attestation: Option<String>,
    pub sign_key: Option<String>,
    pub pq_key: Option<String>,
    pub no_sign: bool,
    pub out: Option<String>,
}

pub struct VerifyOpts {
    pub bundle: String,
    pub pubkey: Option<String>,
    pub pq_pubkey: Option<String>,
    pub require_signature: bool,
    pub require_pq: bool,
    pub require_tier: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bundle {
    pub schema: String,
    pub body: BundleBody,
    pub bundle_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<BundleSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleSignature {
    pub signer: String,
    pub sig: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pq_pubkey: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pq_sig: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleBody {
    pub generated_at_ms: u64,
    pub tool_version: String,
    pub claim_boundary: ClaimBoundary,
    pub gates: GatesEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimBoundary {
    pub enforced_gates_count: u8,
    pub claims_full_84_permf: bool,
    pub note: String,
    pub mappings: Vec<GateMapping>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateMapping {
    pub gate: String,
    pub title: String,
    pub playbox_reqs: Vec<String>,
    pub nist_ai_rmf: Vec<String>,
    pub eu_ai_act: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatesEvidence {
    pub gate_1_capability_and_grant_bounds: Gate1Summary,
    pub gate_2_confinement_and_egress: Gate2Summary,
    pub gate_3_tamper_evident_logs: Gate3Summary,
    pub gate_4_sealed_evaluation_gates: Gate4Summary,
    pub gate_5_identity_and_release_integrity: Gate5Summary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Gate1Summary {
    pub joins_accepted: u64,
    pub joins_refused: u64,
    pub revocations: u64,
    pub capability_calls: u64,
    pub capability_results: u64,
    pub capability_cancelled: u64,
    pub refusals: u64,
    pub room_log_members: Vec<String>,
    pub room_log_banned: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Gate2Summary {
    pub tasks_executed: u64,
    pub tasks_by_runner: BTreeMap<String, u64>,
    pub tasks_cancelled: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EgressSummary {
    pub requests: u64,
    pub denied: u64,
    pub upstreams: Vec<String>,
    pub entries_sha256: String,
    pub entries: Vec<EgressLogEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Gate3Summary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_log: Option<AuditEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_log: Option<RoomLogEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEvidence {
    pub entries: u64,
    pub head: String,
    pub host_key: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomLogEvidence {
    pub root_pubkey: String,
    pub len: u64,
    pub head: String,
    pub committed: u64,
    pub pq_committed: u64,
    pub epoch: u64,
    pub leader: String,
    pub hosts: Vec<String>,
    pub k: usize,
    pub equivocators: Vec<String>,
    pub raw: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Gate4Summary {
    pub room_promotions: Vec<PromotionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge_receipts: Option<ChallengeReceiptsEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attested_receipts: Vec<AttestedReceiptEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttestedReceiptEvidence {
    pub instance: String,
    pub base_commit: String,
    pub runtime: String,
    pub payload_sha256: String,
    pub pubkey_hex: String,
    pub pq_verified: bool,
    pub receipt: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromotionSummary {
    pub problem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    pub metric: i64,
    pub strength: u8,
    pub j: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChallengeReceiptsEvidence {
    pub count: u64,
    pub promoted: u64,
    pub rejected: u64,
    pub with_seed_commit: u64,
    pub with_env_hash: u64,
    pub cryptographically_signed: bool,
    pub sha256: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Gate5Summary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attestation: Option<AttestationEvidence>,
    pub release_pins: ReleasePins,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttestationEvidence {
    pub statement: String,
    pub subject: String,
    pub claimed_tier: String,
    pub achieved_tier: String,
    pub platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    pub runner: String,
    pub binary: String,
    pub policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleasePins {
    pub ed25519: Vec<String>,
    pub ml_dsa_65: Vec<String>,
}

fn default_claim_boundary() -> ClaimBoundary {
    ClaimBoundary {
        enforced_gates_count: 5,
        claims_full_84_permf: false,
        note: "Scoped strictly to the 5 runtime-native Playbox gates; does not claim 84/84 full-profile permf enforcement.".into(),
        mappings: vec![
            GateMapping {
                gate: "gate_1_capability_and_grant_bounds".into(),
                title: "Capability & grant bounds".into(),
                playbox_reqs: vec!["GRANT-1..4".into(), "CAP-1..8".into()],
                nist_ai_rmf: vec!["GOVERN 1.2".into(), "MANAGE 2.4".into()],
                eu_ai_act: vec!["Art. 9".into(), "Art. 14".into()],
            },
            GateMapping {
                gate: "gate_2_confinement_and_egress".into(),
                title: "MicroVM & Linux confinement + BOX-7 egress".into(),
                playbox_reqs: vec!["BOX-2..8".into()],
                nist_ai_rmf: vec!["GOVERN 1.6".into(), "MEASURE 2.7".into()],
                eu_ai_act: vec!["Art. 15".into()],
            },
            GateMapping {
                gate: "gate_3_tamper_evident_logs".into(),
                title: "Tamper-evident hash-chained logs & hybrid PQ receipts".into(),
                playbox_reqs: vec!["ROOM-9..17".into()],
                nist_ai_rmf: vec!["GOVERN 1.4".into(), "MEASURE 2.13".into()],
                eu_ai_act: vec!["Art. 12".into(), "Art. 19".into()],
            },
            GateMapping {
                gate: "gate_4_sealed_evaluation_gates".into(),
                title: "Sealed anti-Goodhart evaluation gates".into(),
                playbox_reqs: vec!["ROOM-16".into(), "ROOM-17".into()],
                nist_ai_rmf: vec!["MEASURE 2.1".into(), "MEASURE 2.5".into()],
                eu_ai_act: vec!["Art. 9".into(), "Art. 15".into()],
            },
            GateMapping {
                gate: "gate_5_identity_and_release_integrity".into(),
                title: "Hardware-bound identity & release integrity".into(),
                playbox_reqs: vec!["IDENT-1..9".into(), "REL-1..9".into()],
                nist_ai_rmf: vec!["GOVERN 1.5".into(), "MANAGE 4.1".into()],
                eu_ai_act: vec!["Art. 13".into(), "Art. 15".into()],
            },
        ],
    }
}

fn sha256_arr(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha256_arr(bytes))
}

fn synthesize_room_log_from_receipts(
    state_dir: &Path,
    sign_key_path: &Path,
    receipt_paths: &[String],
    tier_u8: u8,
) -> Result<Option<PathBuf>> {
    let rk = state_dir.join("root.key");
    let ek = state_dir.join("endpoint.key");
    if !rk.is_file() || !ek.is_file() || !sign_key_path.is_file() || receipt_paths.is_empty() {
        return Ok(None);
    }
    let root_sk = SigningKey::from_bytes(&playbox_net::load_seed_hex(&rk)?);
    let host1_sk = SigningKey::from_bytes(&playbox_net::load_seed_hex(&ek)?);
    let host2_sk = SigningKey::from_bytes(&playbox_net::load_seed_hex(sign_key_path)?);
    let host1_pk = host1_sk.verifying_key().to_bytes();
    let host2_pk = host2_sk.verifying_key().to_bytes();
    if host1_pk == host2_pk {
        return Ok(None);
    }
    let now = store::now_ms();
    let cfg = Config {
        epoch: 0,
        leader: host1_pk,
        hosts: BTreeSet::from([host1_pk, host2_pk]),
        k: 2,
    };
    let mut log = RoomLog::genesis(&root_sk, cfg, now).map_err(|e| anyhow!("{e}"))?;
    let room = log.room();
    let mut seen_problems = BTreeSet::new();

    for rpath in receipt_paths {
        let raw = std::fs::read_to_string(rpath).with_context(|| format!("reading {rpath}"))?;
        let doc: Value = serde_json::from_str(&raw)
            .with_context(|| format!("parsing run receipt {rpath} as JSON"))?;
        let summary = crate::trust::verify_receipt_value(&doc, None, None, false)
            .with_context(|| format!("verifying run receipt {rpath}"))?;
        let resolved = doc["payload"]["sealed_gate"]["holdout_resolved"].as_bool() == Some(true);
        if !resolved {
            continue;
        }
        let inst = doc["payload"]["instance"].as_str().unwrap_or("unknown");
        let base_commit = doc["payload"]["base_commit"].as_str().unwrap_or("base");
        let problem = sha256_arr(format!("playbox.problem.v1:{inst}:{base_commit}").as_bytes());
        let cand_hex = doc["payload"]["router"]["winner_sha256"]
            .as_str()
            .filter(|s| s.len() == 64)
            .or_else(|| summary["payload_sha256"].as_str())
            .unwrap_or("");
        let candidate =
            roomlog::unhex::<32>(cand_hex).unwrap_or_else(|| sha256_arr(cand_hex.as_bytes()));
        if candidate == [0u8; 32] {
            continue;
        }
        let model_name = doc["payload"]["router"]["candidates"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|c| c["model"].as_str())
            .unwrap_or("model");
        let weights = sha256_arr(format!("playbox.weights.v1:{model_name}").as_bytes());
        let pol_hex = doc["payload"]["isolation"]["policy_digest"]
            .as_str()
            .unwrap_or("");
        let env1 = roomlog::unhex::<32>(pol_hex)
            .unwrap_or_else(|| sha256_arr(b"playbox.env.openshell.v1"));
        let env2 = sha256_arr(b"playbox.env.sealed_holdout_gate.v1");

        if seen_problems.insert((problem, candidate)) {
            if !log.frontier(None).contains_key(&problem) {
                log.register_problem(
                    &host1_sk,
                    &Register {
                        problem,
                        rule: Rule {
                            j: 2,
                            noise_ppm: 50_000,
                            min_gain_ppm: 100_000,
                        },
                        baseline: 500_000,
                    },
                    now,
                )
                .map_err(|e| anyhow!("{e}"))?;
            }
            if log
                .frontier(None)
                .get(&problem)
                .is_some_and(|(_, f)| f.metric < 1_000_000)
            {
                let r1 = PromoReceipt::sign(
                    &room, &host1_sk, &problem, &candidate, &weights, env1, tier_u8, 1_000_000,
                );
                let r2 = PromoReceipt::sign(
                    &room, &host2_sk, &problem, &candidate, &weights, env2, tier_u8, 1_000_000,
                );
                log.promote(
                    &host1_sk,
                    &Promotion {
                        problem,
                        candidate,
                        weights,
                        receipts: vec![r1, r2],
                    },
                    now,
                )
                .map_err(|e| anyhow!("{e}"))?;
            }
        }
    }

    let cp = log.checkpoint();
    let mut sc = log
        .sign_checkpoint(&host1_sk, cp.clone())
        .map_err(|e| anyhow!("{e}"))?;
    let sc2 = log
        .sign_checkpoint(&host2_sk, cp)
        .map_err(|e| anyhow!("{e}"))?;
    log.merge(&mut sc, &sc2);

    let mut out = roomlog::text::to_text(&log);
    let sigs_map: serde_json::Map<String, Value> = sc
        .sigs
        .iter()
        .map(|(k, s)| (roomlog::hex(k), Value::from(roomlog::hex(s))))
        .collect();
    let pq_map: serde_json::Map<String, Value> = sc
        .pq_sigs
        .iter()
        .map(|(k, pq)| {
            (
                roomlog::hex(k),
                serde_json::json!({ "pk": roomlog::hex(&pq.pk), "sig": roomlog::hex(&pq.sig) }),
            )
        })
        .collect();
    let cert_line = serde_json::json!({
        "cert": {
            "cp": {
                "epoch": sc.cp.epoch,
                "size": sc.cp.size,
                "head": roomlog::hex(&sc.cp.head),
            },
            "sigs": sigs_map,
            "pq_sigs": pq_map,
        }
    });
    out.push_str(&cert_line.to_string());
    out.push('\n');

    let out_path = state_dir.join("roomlog.jsonl");
    std::fs::write(&out_path, out).with_context(|| format!("writing {}", out_path.display()))?;
    Ok(Some(out_path))
}

fn canonical_digest(body: &BundleBody) -> Result<String> {
    let json = serde_json::to_vec(body).context("serializing bundle body")?;
    let mut h = Sha256::new();
    h.update(BODY_DOMAIN);
    h.update(&json);
    Ok(hex(&h.finalize()))
}

fn sig_message(bundle_digest: &str) -> Vec<u8> {
    let mut msg = Vec::with_capacity(SIG_DOMAIN.len() + bundle_digest.len());
    msg.extend_from_slice(SIG_DOMAIN);
    msg.extend_from_slice(bundle_digest.as_bytes());
    msg
}

fn summarize_audit_entries(entries: &[Value], g1: &mut Gate1Summary, g2: &mut Gate2Summary) {
    for v in entries {
        match v["event"].as_str().unwrap_or("") {
            "join" => {
                if v["f"]["ok"].as_bool() == Some(true) {
                    g1.joins_accepted += 1;
                } else {
                    g1.joins_refused += 1;
                }
            }
            "revoke" => g1.revocations += 1,
            "capability_call" => g1.capability_calls += 1,
            "capability_result" => g1.capability_results += 1,
            "capability_cancelled" => g1.capability_cancelled += 1,
            "refused" | "conn_refused" => g1.refusals += 1,
            "task" => {
                g2.tasks_executed += 1;
                let runner = v["f"]["runner"].as_str().unwrap_or("unknown").to_string();
                *g2.tasks_by_runner.entry(runner).or_insert(0) += 1;
            }
            "task_cancelled" => g2.tasks_cancelled += 1,
            _ => {}
        }
    }
}

fn check_egress_entry_hygiene(e: &EgressLogEntry) -> Result<()> {
    let suspicious = [
        e.path.as_str(),
        e.denied.as_deref().unwrap_or(""),
        e.upstream.as_str(),
    ];
    for field in suspicious {
        let lower = field.to_ascii_lowercase();
        if field.contains("/t/")
            || lower.contains("authorization:")
            || lower.contains("x-api-key:")
            || lower.contains("sk-")
            || lower.contains("ghp_")
        {
            bail!("egress log entry contains potential credential or capability token");
        }
    }
    Ok(())
}

fn parse_egress_log(raw: &str) -> Result<EgressSummary> {
    let mut entries = Vec::new();
    let mut upstreams = BTreeSet::new();
    let mut denied = 0u64;
    let mut canonical = String::new();
    for (idx, line) in raw.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let e: EgressLogEntry = serde_json::from_str(line)
            .with_context(|| format!("egress log line {}: invalid JSON", idx + 1))?;
        check_egress_entry_hygiene(&e)?;
        if e.denied.is_some() || e.status >= 400 {
            denied += 1;
        }
        upstreams.insert(e.upstream.clone());
        canonical.push_str(&serde_json::to_string(&e)?);
        canonical.push('\n');
        entries.push(e);
    }
    Ok(EgressSummary {
        requests: entries.len() as u64,
        denied,
        upstreams: upstreams.into_iter().collect(),
        entries_sha256: sha256_hex(canonical.as_bytes()),
        entries,
    })
}

fn parse_challenge_receipts(raw: &str) -> Result<ChallengeReceiptsEvidence> {
    let mut lines = Vec::new();
    let (mut promoted, mut rejected, mut with_seed_commit, mut with_env_hash) = (0, 0, 0, 0);
    let mut canonical = String::new();
    for (idx, line) in raw.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let v: Value = serde_json::from_str(line)
            .with_context(|| format!("challenge receipt line {}: invalid JSON", idx + 1))?;
        if !v.is_object() {
            bail!("challenge receipt line {} is not a JSON object", idx + 1);
        }
        match v["verdict"].as_str() {
            Some("promote") => promoted += 1,
            Some("reject") => rejected += 1,
            _ => {}
        }
        if v.get("seed_commit").is_some_and(|s| !s.is_null()) {
            with_seed_commit += 1;
        }
        if v.get("env_hash")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        {
            with_env_hash += 1;
        }
        canonical.push_str(line.trim());
        canonical.push('\n');
        lines.push(line.trim().to_string());
    }
    Ok(ChallengeReceiptsEvidence {
        count: lines.len() as u64,
        promoted,
        rejected,
        with_seed_commit,
        with_env_hash,
        cryptographically_signed: false,
        sha256: sha256_hex(canonical.as_bytes()),
        lines,
    })
}

fn load_attestation_text(path: &Path) -> Result<String> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if attest::Statement::parse(&raw).is_ok() {
        return Ok(raw);
    }
    store::read_latest_attestation(path)
        .with_context(|| format!("loading attestation from {}", path.display()))
}

fn verify_attestation_text(text: &str) -> Result<AttestationEvidence> {
    let home = store::home();
    let pins = platform::PinStore::load(&platform::pins_path(&home))?;
    let boot = platform::boot_reference_from_env().map_err(anyhow::Error::msg)?;
    let vs = platform::verifiers(pins, boot).map_err(anyhow::Error::msg)?;
    let refs: Vec<&dyn PlatformVerifier> = vs.iter().map(|v| v.as_ref()).collect();
    let verified = attest::verify(text, &refs).map_err(anyhow::Error::msg)?;
    let m = &verified.statement.measurements;
    Ok(AttestationEvidence {
        statement: verified.text,
        subject: hex(&verified.statement.subject),
        claimed_tier: verified.statement.claimed.to_string(),
        achieved_tier: verified.achieved.to_string(),
        platform: verified.statement.platform,
        device: verified.device,
        runner: m.runner.clone(),
        binary: m.binary.clone(),
        policy: m.policy.clone(),
    })
}

fn load_pq_sk(path: &Path) -> Result<Vec<u8>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading PQ signing key {}", path.display()))?;
    let t = text.trim();
    if !t.len().is_multiple_of(2) || !t.is_ascii() {
        bail!("PQ signing key {} must be even-length hex", path.display());
    }
    let bytes: Vec<u8> = (0..t.len() / 2)
        .map(|i| {
            u8::from_str_radix(&t[i * 2..i * 2 + 2], 16)
                .map_err(|_| anyhow!("invalid hex in PQ signing key {}", path.display()))
        })
        .collect::<Result<_>>()?;
    if bytes.len() != pq::MLDSA65_SK_LEN {
        bail!(
            "PQ signing key {} must be {} bytes",
            path.display(),
            pq::MLDSA65_SK_LEN
        );
    }
    Ok(bytes)
}

fn parse_pq_pk_hex(hex_str: &str) -> Result<Vec<u8>> {
    let t = hex_str.trim();
    if !t.len().is_multiple_of(2) || !t.is_ascii() {
        bail!("ML-DSA-65 public key must be even-length hex");
    }
    let bytes: Vec<u8> = (0..t.len() / 2)
        .map(|i| {
            u8::from_str_radix(&t[i * 2..i * 2 + 2], 16)
                .map_err(|_| anyhow!("invalid hex in ML-DSA-65 public key"))
        })
        .collect::<Result<_>>()?;
    if bytes.len() != pq::MLDSA65_PK_LEN {
        bail!(
            "ML-DSA-65 public key must be {} bytes (got {})",
            pq::MLDSA65_PK_LEN,
            bytes.len()
        );
    }
    Ok(bytes)
}

pub fn export(o: ExportOpts) -> Result<()> {
    let state_dir = o.state.as_deref().map(PathBuf::from);
    let audit_path = o.audit.as_deref().map(PathBuf::from).or_else(|| {
        state_dir
            .as_ref()
            .map(|d| d.join("audit.log"))
            .filter(|p| p.is_file())
    });
    let mut roomlog_path = o.room_log.as_deref().map(PathBuf::from).or_else(|| {
        state_dir
            .as_ref()
            .map(|d| d.join("roomlog.jsonl"))
            .filter(|p| p.is_file())
    });
    let egress_path = o.egress_log.as_deref().map(PathBuf::from).or_else(|| {
        state_dir
            .as_ref()
            .map(|d| d.join("egress.jsonl"))
            .filter(|p| p.is_file())
    });
    let challenge_path = o.challenge_log.as_deref().map(PathBuf::from);
    let attest_path = o.attestation.as_deref().map(PathBuf::from);

    let attestation = match &attest_path {
        Some(p) => {
            let raw = load_attestation_text(p)?;
            Some(verify_attestation_text(&raw)?)
        }
        None => None,
    };
    let tier_u8 = attestation
        .as_ref()
        .and_then(|a| Tier::parse(&a.achieved_tier).ok())
        .map_or(0, |t| match t {
            Tier::T0 => 0,
            Tier::T1 => 1,
            Tier::T2 => 2,
            Tier::T3 => 3,
        });

    if roomlog_path.is_none() {
        if let (Some(d), Some(sk_path)) = (&state_dir, &o.sign_key) {
            roomlog_path =
                synthesize_room_log_from_receipts(d, Path::new(sk_path), &o.receipts, tier_u8)?;
        }
    }

    if audit_path.is_none()
        && roomlog_path.is_none()
        && egress_path.is_none()
        && challenge_path.is_none()
        && o.receipts.is_empty()
        && attest_path.is_none()
    {
        bail!(
            "no evidence inputs found (pass --state DIR with logs or at least one artifact flag)"
        );
    }

    let mut g1 = Gate1Summary::default();
    let mut g2 = Gate2Summary::default();
    let mut g3 = Gate3Summary::default();
    let mut g4 = Gate4Summary::default();

    if let Some(p) = &audit_path {
        let raw = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let lines: Vec<String> = raw.lines().map(String::from).collect();
        let (summary, entries) = playbox_net::audit::verify_lines(&lines, None)
            .with_context(|| format!("verifying audit log {}", p.display()))?;
        summarize_audit_entries(&entries, &mut g1, &mut g2);
        g3.audit_log = Some(AuditEvidence {
            entries: summary.entries,
            head: summary.head,
            host_key: summary.host_key,
            lines,
        });
    }

    if let Some(p) = &roomlog_path {
        let root_bytes = if let Some(r) = &o.root {
            roomlog::unhex::<32>(r.trim())
                .ok_or_else(|| anyhow!("--root must be 64 hex characters"))?
        } else if let Some(d) = &state_dir {
            let rk = d.join("root.key");
            if rk.is_file() {
                let seed = playbox_net::load_seed_hex(&rk)?;
                SigningKey::from_bytes(&seed).verifying_key().to_bytes()
            } else {
                bail!(
                    "--root HEX is required when {} has no root.key",
                    d.display()
                );
            }
        } else {
            bail!("--root HEX is required with --room-log");
        };
        let raw = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let st = playbox_net::roomsync::read_status_text(root_bytes, &raw)
            .with_context(|| format!("verifying room log {}", p.display()))?
            .ok_or_else(|| anyhow!("room log {} is empty", p.display()))?;
        if !st.equivocators.is_empty() {
            bail!(
                "refusing to export compliant bundle: equivocation proven against {}",
                st.equivocators.join(", ")
            );
        }
        g1.room_log_members.clone_from(&st.members);
        g1.room_log_banned.clone_from(&st.banned);
        g4.room_promotions = st
            .problems
            .iter()
            .map(|pr| PromotionSummary {
                problem: pr.problem.clone(),
                candidate: pr.candidate.clone(),
                metric: pr.metric,
                strength: pr.strength,
                j: pr.j,
            })
            .collect();
        g3.room_log = Some(RoomLogEvidence {
            root_pubkey: hex(&root_bytes),
            len: st.len,
            head: st.head,
            committed: st.committed,
            pq_committed: st.pq_committed,
            epoch: st.epoch,
            leader: st.leader,
            hosts: st.hosts,
            k: st.k,
            equivocators: st.equivocators,
            raw,
        });
    }

    if let Some(p) = &egress_path {
        let raw = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        g2.egress = Some(parse_egress_log(&raw)?);
    }

    if let Some(p) = &challenge_path {
        let raw = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        g4.challenge_receipts = Some(parse_challenge_receipts(&raw)?);
    }

    for rpath in &o.receipts {
        let raw = std::fs::read_to_string(rpath).with_context(|| format!("reading {rpath}"))?;
        let doc: Value = serde_json::from_str(&raw)
            .with_context(|| format!("parsing run receipt {rpath} as JSON"))?;
        let summary = crate::trust::verify_receipt_value(&doc, None, None, false)
            .with_context(|| format!("verifying run receipt {rpath}"))?;
        g4.attested_receipts.push(AttestedReceiptEvidence {
            instance: doc["payload"]["instance"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            base_commit: doc["payload"]["base_commit"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            runtime: doc["payload"]["runtime"].as_str().unwrap_or("").to_string(),
            payload_sha256: summary["payload_sha256"].as_str().unwrap_or("").to_string(),
            pubkey_hex: summary["pubkey_hex"].as_str().unwrap_or("").to_string(),
            pq_verified: summary["pq_verified"].as_bool() == Some(true),
            receipt: doc,
        });
    }

    let body = BundleBody {
        generated_at_ms: store::now_ms(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        claim_boundary: default_claim_boundary(),
        gates: GatesEvidence {
            gate_1_capability_and_grant_bounds: g1,
            gate_2_confinement_and_egress: g2,
            gate_3_tamper_evident_logs: g3,
            gate_4_sealed_evaluation_gates: g4,
            gate_5_identity_and_release_integrity: Gate5Summary {
                attestation,
                release_pins: ReleasePins {
                    ed25519: crate::update::pinned_keys(),
                    ml_dsa_65: crate::update::pinned_pq_keys(),
                },
            },
        },
    };
    let bundle_digest = canonical_digest(&body)?;

    let sign_seed = if o.no_sign {
        None
    } else if let Some(p) = &o.sign_key {
        Some(playbox_net::load_seed_hex(Path::new(p))?)
    } else if let Some(d) = &state_dir {
        let ep = d.join("endpoint.key");
        if ep.is_file() {
            Some(playbox_net::load_seed_hex(&ep)?)
        } else {
            None
        }
    } else {
        None
    };
    if o.pq_key.is_some() && sign_seed.is_none() {
        bail!("--pq-key requires an Ed25519 signing key (--sign-key or STATE/endpoint.key)");
    }

    let signature = if let Some(seed) = sign_seed {
        let sk = SigningKey::from_bytes(&seed);
        let msg = sig_message(&bundle_digest);
        let sig = sk.sign(&msg);
        let (pq_pubkey, pq_sig) = if let Some(pq_path) = &o.pq_key {
            let pq_sk = load_pq_sk(Path::new(pq_path))?;
            let pq_pk = pq::pk_from_sk_mldsa65(&pq_sk)?;
            let pq_s = pq::sign_mldsa65(&pq_sk, bundle_digest.as_bytes(), PQ_CTX)?;
            (Some(hex(&pq_pk)), Some(hex(&pq_s)))
        } else {
            let (pq_pk, pq_sk) = pq::derive_mldsa65_from_ed25519_seed(&seed);
            let pq_s = pq::sign_mldsa65(&pq_sk, bundle_digest.as_bytes(), PQ_CTX)?;
            (Some(hex(&pq_pk)), Some(hex(&pq_s)))
        };
        Some(BundleSignature {
            signer: hex(&sk.verifying_key().to_bytes()),
            sig: hex(&sig.to_bytes()),
            pq_pubkey,
            pq_sig,
        })
    } else {
        None
    };

    let bundle = Bundle {
        schema: BUNDLE_SCHEMA.to_string(),
        body,
        bundle_digest,
        signature,
    };
    let rendered = serde_json::to_string_pretty(&bundle)?;
    match &o.out {
        Some(path) => {
            std::fs::write(path, format!("{rendered}\n"))
                .with_context(|| format!("writing {path}"))?;
        }
        None => println!("{rendered}"),
    }
    Ok(())
}

pub fn verify(o: VerifyOpts) -> Result<()> {
    let raw = if o.bundle == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading bundle from stdin")?;
        s
    } else {
        std::fs::read_to_string(&o.bundle).with_context(|| format!("reading {}", o.bundle))?
    };
    let raw_val: Value = serde_json::from_str(&raw).context("parsing compliance bundle JSON")?;
    if raw_val
        .get("payload")
        .and_then(|p| p.get("schema"))
        .and_then(Value::as_str)
        == Some(crate::trust::RECEIPT_SCHEMA)
    {
        if o.require_tier.is_some() {
            bail!(
                "--require-tier requires a 5-gate compliance bundle ({BUNDLE_SCHEMA}); this file is a run receipt"
            );
        }
        let summary = crate::trust::verify_receipt_value(
            &raw_val,
            o.pubkey.as_deref(),
            o.pq_pubkey.as_deref(),
            o.require_pq,
        )?;
        if o.json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else {
            println!(
                "attested gate receipt OK: schema={} sha256={} alg={} pq_verified={}",
                crate::trust::RECEIPT_SCHEMA,
                &summary["payload_sha256"].as_str().unwrap_or("")[..16],
                summary["alg"].as_str().unwrap_or(""),
                summary["pq_verified"].as_bool().unwrap_or(false),
            );
        }
        return Ok(());
    }
    let bundle: Bundle =
        serde_json::from_value(raw_val).context("parsing compliance bundle JSON")?;
    if bundle.schema != BUNDLE_SCHEMA {
        bail!(
            "unsupported bundle schema {:?}; expected {BUNDLE_SCHEMA:?}",
            bundle.schema
        );
    }
    let expected_digest = canonical_digest(&bundle.body)?;
    if bundle.bundle_digest != expected_digest {
        bail!("bundle digest mismatch (body was modified after export)");
    }

    let cb = &bundle.body.claim_boundary;
    if cb.enforced_gates_count != 5 || cb.claims_full_84_permf || cb.mappings.len() != 5 {
        bail!("claim boundary violates strict 5-gate v0.4 policy");
    }

    if let Some(expected_pk) = &o.pubkey {
        let parsed: [u8; 32] = unhex(expected_pk.trim()).map_err(|e| anyhow!("--pubkey: {e}"))?;
        let sig = bundle
            .signature
            .as_ref()
            .ok_or_else(|| anyhow!("bundle is unsigned, but --pubkey was required"))?;
        let actual: [u8; 32] = unhex(&sig.signer).map_err(|e| anyhow!("bundle signer: {e}"))?;
        if actual != parsed {
            bail!(
                "bundle signer {} does not match --pubkey {}",
                sig.signer,
                hex(&parsed)
            );
        }
    }
    if o.require_signature && bundle.signature.is_none() {
        bail!("bundle is unsigned (--require-signature)");
    }

    let mut bundle_sig_verified = false;
    let mut bundle_pq_verified = false;
    if let Some(s) = &bundle.signature {
        let pk_bytes: [u8; 32] = unhex(&s.signer).map_err(|e| anyhow!("bundle signer: {e}"))?;
        let sig_bytes: [u8; 64] = unhex(&s.sig).map_err(|e| anyhow!("bundle sig: {e}"))?;
        let vk = VerifyingKey::from_bytes(&pk_bytes).map_err(|_| anyhow!("invalid signer key"))?;
        vk.verify(
            &sig_message(&bundle.bundle_digest),
            &Signature::from_bytes(&sig_bytes),
        )
        .map_err(|_| anyhow!("bundle Ed25519 signature verification failed"))?;
        bundle_sig_verified = true;

        if let Some(pq_sig_hex) = &s.pq_sig {
            let pq_pk_hex = s
                .pq_pubkey
                .as_deref()
                .ok_or_else(|| anyhow!("bundle has pq_sig without pq_pubkey"))?;
            let pq_pk = parse_pq_pk_hex(pq_pk_hex)?;
            let pq_sig = parse_hex_vec(pq_sig_hex, "bundle pq_sig")?;
            if !pq::verify_mldsa65(&pq_pk, bundle.bundle_digest.as_bytes(), &pq_sig, PQ_CTX) {
                bail!("bundle ML-DSA-65 signature verification failed");
            }
            if let Some(want_pq) = &o.pq_pubkey {
                let want = parse_pq_pk_hex(want_pq)?;
                if pq_pk != want {
                    bail!("bundle ML-DSA-65 public key does not match --pq-pubkey");
                }
            }
            bundle_pq_verified = true;
        } else if o.pq_pubkey.is_some() {
            bail!("--pq-pubkey was given, but bundle carries no ML-DSA-65 signature");
        }
    } else if o.pq_pubkey.is_some() {
        bail!("--pq-pubkey was given, but bundle is unsigned");
    }

    let g = &bundle.body.gates;
    let mut expected_g1 = Gate1Summary::default();
    let mut expected_g2 = Gate2Summary::default();

    if let Some(a) = &g.gate_3_tamper_evident_logs.audit_log {
        let (summary, entries) = playbox_net::audit::verify_lines(&a.lines, None)
            .context("embedded audit_log verification failed")?;
        if summary.entries != a.entries || summary.head != a.head || summary.host_key != a.host_key
        {
            bail!("embedded audit_log summary does not match verified lines");
        }
        summarize_audit_entries(&entries, &mut expected_g1, &mut expected_g2);
    }

    if let Some(r) = &g.gate_3_tamper_evident_logs.room_log {
        let root_bytes = roomlog::unhex::<32>(&r.root_pubkey)
            .ok_or_else(|| anyhow!("invalid room_log root_pubkey"))?;
        let st = playbox_net::roomsync::read_status_text(root_bytes, &r.raw)
            .context("embedded room_log verification failed")?
            .ok_or_else(|| anyhow!("embedded room_log is empty"))?;
        if !st.equivocators.is_empty() {
            bail!(
                "embedded room_log proves equivocation: {}",
                st.equivocators.join(", ")
            );
        }
        if st.len != r.len
            || st.head != r.head
            || st.committed != r.committed
            || st.pq_committed != r.pq_committed
            || st.epoch != r.epoch
            || st.leader != r.leader
            || st.hosts != r.hosts
            || st.k != r.k
            || st.equivocators != r.equivocators
        {
            bail!("embedded room_log status does not match verified replay");
        }
        expected_g1.room_log_members = st.members;
        expected_g1.room_log_banned = st.banned;
        let expected_promos: Vec<PromotionSummary> = st
            .problems
            .iter()
            .map(|pr| PromotionSummary {
                problem: pr.problem.clone(),
                candidate: pr.candidate.clone(),
                metric: pr.metric,
                strength: pr.strength,
                j: pr.j,
            })
            .collect();
        if expected_promos != g.gate_4_sealed_evaluation_gates.room_promotions {
            bail!("gate_4 room_promotions do not match verified RoomLog frontier");
        }
    } else if !g.gate_4_sealed_evaluation_gates.room_promotions.is_empty() {
        bail!("gate_4 lists room_promotions without an embedded verifiable room_log");
    }

    if expected_g1 != g.gate_1_capability_and_grant_bounds {
        bail!("gate_1 summary does not match verified audit/room logs");
    }

    if let Some(eg) = &g.gate_2_confinement_and_egress.egress {
        let mut canonical = String::new();
        let mut upstreams = BTreeSet::new();
        let mut denied = 0u64;
        for entry in &eg.entries {
            check_egress_entry_hygiene(entry)?;
            if entry.denied.is_some() || entry.status >= 400 {
                denied += 1;
            }
            upstreams.insert(entry.upstream.clone());
            canonical.push_str(&serde_json::to_string(entry)?);
            canonical.push('\n');
        }
        let recomputed_hash = sha256_hex(canonical.as_bytes());
        let upstream_vec: Vec<String> = upstreams.into_iter().collect();
        if eg.requests != eg.entries.len() as u64
            || eg.denied != denied
            || eg.upstreams != upstream_vec
            || eg.entries_sha256 != recomputed_hash
        {
            bail!("gate_2 egress summary does not match embedded entries");
        }
        expected_g2.egress = Some(eg.clone());
    }
    if expected_g2 != g.gate_2_confinement_and_egress {
        bail!("gate_2 summary does not match verified audit/egress logs");
    }

    if let Some(ch) = &g.gate_4_sealed_evaluation_gates.challenge_receipts {
        if ch.cryptographically_signed {
            bail!("gate_4 challenge_receipts cannot claim cryptographic signature verification");
        }
        let joined = ch.lines.join("\n");
        let recomputed = parse_challenge_receipts(&joined)?;
        if &recomputed != ch {
            bail!("gate_4 challenge_receipts summary does not match embedded lines");
        }
    }

    for (idx, ar) in g
        .gate_4_sealed_evaluation_gates
        .attested_receipts
        .iter()
        .enumerate()
    {
        let summary = crate::trust::verify_receipt_value(&ar.receipt, None, None, o.require_pq)
            .with_context(|| format!("gate_4 attested_receipt[{idx}] failed verification"))?;
        if summary["payload_sha256"].as_str() != Some(ar.payload_sha256.as_str())
            || summary["pubkey_hex"].as_str() != Some(ar.pubkey_hex.as_str())
            || summary["pq_verified"].as_bool() != Some(ar.pq_verified)
            || ar.receipt["payload"]["instance"].as_str().unwrap_or("") != ar.instance
            || ar.receipt["payload"]["base_commit"].as_str().unwrap_or("") != ar.base_commit
            || ar.receipt["payload"]["runtime"].as_str().unwrap_or("") != ar.runtime
        {
            bail!("gate_4 attested_receipt[{idx}] summary does not match signed receipt");
        }
    }

    if o.require_pq {
        let room_pq = g
            .gate_3_tamper_evident_logs
            .room_log
            .as_ref()
            .is_some_and(|r| r.pq_committed > 0);
        let receipts_pq = !g
            .gate_4_sealed_evaluation_gates
            .attested_receipts
            .is_empty()
            && g.gate_4_sealed_evaluation_gates
                .attested_receipts
                .iter()
                .all(|r| r.pq_verified);
        if bundle.signature.is_some() && !bundle_pq_verified {
            bail!("--require-pq: signed bundle does not carry a verified ML-DSA-65 signature");
        }
        if let Some(r) = &g.gate_3_tamper_evident_logs.room_log {
            if r.pq_committed == 0 {
                bail!("--require-pq: embedded room_log has 0 PQ-committed entries");
            }
        }
        if !bundle_pq_verified && !room_pq && !receipts_pq {
            bail!(
                "--require-pq: neither bundle ML-DSA-65 signature, PQ-committed RoomLog, nor PQ-signed run receipt present"
            );
        }
    }

    let required_tier = o
        .require_tier
        .as_deref()
        .map(Tier::parse)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    match (
        &g.gate_5_identity_and_release_integrity.attestation,
        required_tier,
    ) {
        (None, Some(req)) => bail!("--require-tier {req}: bundle carries no attestation statement"),
        (Some(att), req) => {
            let re = verify_attestation_text(&att.statement)?;
            if &re != att {
                bail!("gate_5 attestation fields do not match verified statement");
            }
            if let Some(want) = req {
                let got = Tier::parse(&re.achieved_tier).map_err(anyhow::Error::msg)?;
                if got < want {
                    bail!("host attestation achieved {got}, below required {want}");
                }
            }
        }
        (None, None) => {}
    }

    if o.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "schema": bundle.schema,
                "bundle_digest": bundle.bundle_digest,
                "signed": bundle_sig_verified,
                "pq_signed": bundle_pq_verified,
                "enforced_gates_count": cb.enforced_gates_count,
                "claims_full_84_permf": cb.claims_full_84_permf,
                "audit_entries": g.gate_3_tamper_evident_logs.audit_log.as_ref().map(|a| a.entries),
                "room_log_len": g.gate_3_tamper_evident_logs.room_log.as_ref().map(|r| r.len),
                "room_log_committed": g.gate_3_tamper_evident_logs.room_log.as_ref().map(|r| r.committed),
                "room_log_pq_committed": g.gate_3_tamper_evident_logs.room_log.as_ref().map(|r| r.pq_committed),
                "egress_requests": g.gate_2_confinement_and_egress.egress.as_ref().map(|e| e.requests),
                "promotions": g.gate_4_sealed_evaluation_gates.room_promotions.len(),
                "attested_receipts": g.gate_4_sealed_evaluation_gates.attested_receipts.len(),
                "achieved_tier": g.gate_5_identity_and_release_integrity.attestation.as_ref().map(|a| a.achieved_tier.clone()),
            }))?
        );
    } else {
        println!(
            "attested gate OK: schema={} digest={} sig={} pq_sig={}",
            bundle.schema,
            &bundle.bundle_digest[..16],
            bundle_sig_verified,
            bundle_pq_verified
        );
        println!(
            "  claim boundary: {}/5 runtime-native gates (claims_full_84_permf={})",
            cb.enforced_gates_count, cb.claims_full_84_permf
        );
        println!(
            "  gate 1 (grants/caps): joins={} refused={} revocations={} cap_calls={} members={}",
            g.gate_1_capability_and_grant_bounds.joins_accepted,
            g.gate_1_capability_and_grant_bounds.joins_refused,
            g.gate_1_capability_and_grant_bounds.revocations,
            g.gate_1_capability_and_grant_bounds.capability_calls,
            g.gate_1_capability_and_grant_bounds.room_log_members.len(),
        );
        println!(
            "  gate 2 (confinement/BOX-7): tasks={} egress_reqs={} egress_denied={}",
            g.gate_2_confinement_and_egress.tasks_executed,
            g.gate_2_confinement_and_egress
                .egress
                .as_ref()
                .map_or(0, |e| e.requests),
            g.gate_2_confinement_and_egress
                .egress
                .as_ref()
                .map_or(0, |e| e.denied),
        );
        println!(
            "  gate 3 (logs/PQ): audit_entries={} room_log_len={} committed={} pq_committed={}",
            g.gate_3_tamper_evident_logs
                .audit_log
                .as_ref()
                .map_or(0, |a| a.entries),
            g.gate_3_tamper_evident_logs
                .room_log
                .as_ref()
                .map_or(0, |r| r.len),
            g.gate_3_tamper_evident_logs
                .room_log
                .as_ref()
                .map_or(0, |r| r.committed),
            g.gate_3_tamper_evident_logs
                .room_log
                .as_ref()
                .map_or(0, |r| r.pq_committed),
        );
        println!(
            "  gate 4 (eval gate): promotions={} attested_receipts={} challenge_receipts={}",
            g.gate_4_sealed_evaluation_gates.room_promotions.len(),
            g.gate_4_sealed_evaluation_gates.attested_receipts.len(),
            g.gate_4_sealed_evaluation_gates
                .challenge_receipts
                .as_ref()
                .map_or(0, |c| c.count),
        );
        println!(
            "  gate 5 (identity/release): tier={} release_pins_ed25519={} release_pins_mldsa65={}",
            g.gate_5_identity_and_release_integrity
                .attestation
                .as_ref()
                .map_or("none", |a| a.achieved_tier.as_str()),
            g.gate_5_identity_and_release_integrity
                .release_pins
                .ed25519
                .len(),
            g.gate_5_identity_and_release_integrity
                .release_pins
                .ml_dsa_65
                .len(),
        );
    }
    Ok(())
}

fn parse_hex_vec(s: &str, label: &str) -> Result<Vec<u8>> {
    let t = s.trim();
    if !t.len().is_multiple_of(2) || !t.is_ascii() {
        bail!("{label} must be even-length hex");
    }
    (0..t.len() / 2)
        .map(|i| {
            u8::from_str_radix(&t[i * 2..i * 2 + 2], 16)
                .map_err(|_| anyhow!("invalid hex in {label}"))
        })
        .collect()
}
