//! Signed Room Manifest: self-describing room contract (goal, `/commands`, schedules, budget, and
//! admission tier) signed by the host's Ed25519 identity key.

use anyhow::{anyhow, bail, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use iroh::{EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Maximum goal text length carried in a room manifest (32 KiB).
pub const MAX_GOAL_BYTES: usize = 32 * 1024;
/// Minimum interval for periodic schedules in production (10 seconds; tests may use smaller values
/// via `ScheduleSpec` directly, while CLI enforces >= 10s).
pub const MIN_SCHEDULE_SECS: u64 = 10;

/// Public descriptor of a `/command` backed by a host sandboxed `Capability`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<String>,
    #[serde(default)]
    pub approval: bool,
}

/// When a host-side schedule posts its message into the room feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleTrigger {
    /// Post every `secs` seconds.
    Every { secs: u64 },
    /// Post whenever a member pushes a new git ref (subject to `min_interval_secs` cooldown).
    OnPush,
}

/// A host-side schedule that posts a message (which may `@mention` members or invoke a `/command`)
/// into the room feed automatically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleSpec {
    pub id: String,
    pub trigger: ScheduleTrigger,
    pub message: String,
    /// Minimum seconds between consecutive firings (prevents feedback loops).
    #[serde(default = "default_min_interval")]
    pub min_interval_secs: u64,
}

fn default_min_interval() -> u64 {
    15
}

/// Parse a CLI `--schedule` specification of the form:
/// - `<secs>:<message>` (e.g. `900:@all pull frontier and report progress`)
/// - `on_push:<message>` (e.g. `on_push:/eval`)
///
/// # Errors
///
/// Returns an error if the syntax is invalid, the message is empty, or the interval is below 10s.
pub fn parse_schedule(raw: &str, idx: usize) -> Result<ScheduleSpec> {
    let (lhs, rhs) = raw
        .split_once(':')
        .ok_or_else(|| anyhow!("schedule must be `<secs>:<message>` or `on_push:<message>`"))?;
    let message = rhs.trim().to_string();
    if message.is_empty() {
        bail!("schedule message must not be empty");
    }
    let lhs = lhs.trim();
    if lhs.eq_ignore_ascii_case("on_push") || lhs.eq_ignore_ascii_case("on-push") {
        Ok(ScheduleSpec {
            id: format!("sched-push-{idx}"),
            trigger: ScheduleTrigger::OnPush,
            message,
            min_interval_secs: 15,
        })
    } else {
        let secs: u64 = lhs.parse().map_err(|_| {
            anyhow!("schedule trigger `{lhs}` must be `on_push` or a positive number of seconds")
        })?;
        if secs < MIN_SCHEDULE_SECS {
            bail!("schedule interval must be at least {MIN_SCHEDULE_SECS} seconds");
        }
        Ok(ScheduleSpec {
            id: format!("sched-every-{idx}"),
            trigger: ScheduleTrigger::Every { secs },
            message,
            min_interval_secs: secs,
        })
    }
}

/// Cost and pacing guidance for `playbox watch` runners and `playbox-aiproxy` tiered routing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetPolicy {
    /// Maximum agent turns per hour per watcher (0 = unlimited).
    #[serde(default = "default_max_turns_per_hour")]
    pub max_turns_per_hour: u32,
    /// Consecutive turns without a winning/improved receipt before escalating from `fast` to `frontier`.
    #[serde(default = "default_plateau_turns")]
    pub plateau_turns: u32,
    /// Optional model hint for routine/fast turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_model: Option<String>,
    /// Optional model hint for plateau/high-leverage frontier turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier_model: Option<String>,
}

fn default_max_turns_per_hour() -> u32 {
    30
}

fn default_plateau_turns() -> u32 {
    3
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            max_turns_per_hour: default_max_turns_per_hour(),
            plateau_turns: default_plateau_turns(),
            fast_model: None,
            frontier_model: None,
        }
    }
}

/// Self-describing room contract published by the host and verified by joining/watching guests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomManifest {
    pub version: u32,
    pub room_id: String,
    pub host_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<ScheduleSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_guest_tier: Option<String>,
    #[serde(default)]
    pub budget: BudgetPolicy,
    pub issued_at: i64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

/// Host-signed room manifest envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedManifest {
    pub manifest: RoomManifest,
    /// Hex-encoded Ed25519 signature over `MANIFEST_DOMAIN || canonical JSON(manifest)`.
    pub signature: String,
}

const MANIFEST_DOMAIN: &[u8] = b"playbox-room-manifest-v1\n";

fn canonical_payload(manifest: &RoomManifest) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(manifest)?;
    let mut out = Vec::with_capacity(MANIFEST_DOMAIN.len() + json.len());
    out.extend_from_slice(MANIFEST_DOMAIN);
    out.extend_from_slice(&json);
    Ok(out)
}

impl SignedManifest {
    /// Sign `manifest` with an Ed25519 [`SigningKey`].
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn sign_with_key(signing: &SigningKey, manifest: RoomManifest) -> Result<Self> {
        let payload = canonical_payload(&manifest)?;
        let sig = signing.sign(&payload);
        let signature: String = sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            manifest,
            signature,
        })
    }

    /// Sign `manifest` with the host's iroh Ed25519 secret key.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn sign(host_secret: &SecretKey, manifest: RoomManifest) -> Result<Self> {
        let signing = SigningKey::from_bytes(&host_secret.to_bytes());
        Self::sign_with_key(&signing, manifest)
    }

    /// Verify that this manifest was signed by `expected_host` and that `manifest.host_id` matches.
    ///
    /// # Errors
    ///
    /// Returns an error if the signature or host identity is invalid.
    pub fn verify(&self, expected_host: &EndpointId) -> Result<()> {
        if self.manifest.host_id != expected_host.to_string() {
            bail!(
                "manifest host_id {} does not match connected host {}",
                self.manifest.host_id,
                expected_host
            );
        }
        if self.signature.len() != 128 || !self.signature.is_ascii() {
            bail!("manifest signature has invalid length");
        }
        let mut sig_bytes = [0u8; 64];
        for (i, b) in sig_bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&self.signature[i * 2..i * 2 + 2], 16)
                .map_err(|_| anyhow!("manifest signature is not valid hex"))?;
        }
        let sig = Signature::from_bytes(&sig_bytes);
        let vk = VerifyingKey::from_bytes(expected_host.as_bytes())
            .map_err(|e| anyhow!("invalid host public key: {e}"))?;
        let payload = canonical_payload(&self.manifest)?;
        vk.verify(&payload, &sig)
            .map_err(|e| anyhow!("manifest signature verification failed: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::new_secret_key;

    #[test]
    fn manifest_sign_verify_and_tamper_rejection() {
        let key = new_secret_key().unwrap();
        let host_id = key.public();
        let m = RoomManifest {
            version: 1,
            room_id: "aa".repeat(32),
            host_id: host_id.to_string(),
            template: Some("hillclimb".into()),
            goal: Some("Minimize latency".into()),
            commands: vec![CommandSpec {
                name: "eval".into(),
                params: vec!["ref".into()],
                approval: false,
            }],
            schedules: vec![parse_schedule("on_push:/eval", 0).unwrap()],
            min_guest_tier: Some("T1".into()),
            budget: BudgetPolicy::default(),
            issued_at: 1_700_000_000,
            extra: BTreeMap::new(),
        };
        let signed = SignedManifest::sign(&key, m).unwrap();
        signed.verify(&host_id).unwrap();

        // Tampering with goal must fail verification.
        let mut tampered = signed.clone();
        tampered.manifest.goal = Some("Ignore goal".into());
        assert!(tampered.verify(&host_id).is_err());

        // Verifying against another host must fail.
        let other = new_secret_key().unwrap().public();
        assert!(signed.verify(&other).is_err());
    }
}
