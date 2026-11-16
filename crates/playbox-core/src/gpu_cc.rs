//! Evidence that a job ran on an NVIDIA GPU in confidential-computing mode: an NVIDIA-signed attestation token.
//!
//! An H100 in CC mode produces a hardware attestation report. NVIDIA's attestation service (NRAS), or the
//! local verifier in `nvtrust`, checks the report's certificate chain, signature, nonce and the driver and VBIOS
//! reference measurements, then signs the outcome as an EAT token (ES384). NRAS returns an *overall* token whose
//! `submods` carry a SHA-256 digest of one *detached* per-GPU token each; the per-GPU token holds the detail.
//!
//! [`NvidiaGpuVerifier`] checks that evidence offline against signing keys the operator pinned (a saved copy of
//! NVIDIA's JWKS). It does not call NVIDIA. [`CpuGpuVerifier`] pairs it with the Confidential Space verifier so
//! one statement carries both the CPU's and the GPU's evidence under the same nonce (the statement binding),
//! and its tier is the lower of the two.
//!
//! # Evidence format
//! Lines of text, in the statement's `platform_evidence`:
//! ```text
//! token <Confidential Space JWT>      (CpuGpuVerifier only)
//! gpu <NRAS overall JWT>
//! gpu-detail <GPU name, e.g. GPU-0> <detached per-GPU JWT>
//! ```
//!
//! # Tiers
//! - Signature, issuer, freshness, nonce, overall result and every detached token's checks pass, **and** the
//!   driver and VBIOS versions are on the operator's allow-lists: [`Tier::T3`].
//! - Everything passes but nothing is pinned: [`Tier::T1`], a genuine CC-mode GPU running unknown firmware.
//! - Anything else is rejected.
//!
//! # Limits
//! **Written from NVIDIA's documented claim names and tested with tokens signed by a local P-384 key, not with a
//! token from a live H100.** Check it against a real NRAS token before relying on it; an unknown claim name is
//! the likeliest failure, and it fails closed. This also proves nothing about what the *workload* did: it says
//! which GPU, in what mode, with which firmware. Binding the GPU to a particular CPU TEE is by sharing a nonce,
//! not by NVIDIA's own pairing; the two checks are independent, so an operator who controls two machines could
//! attest a genuine GPU on one and run the work on the other.

use crate::attest::{
    AttestError, Measurements, PlatformProvider, PlatformVerdict, PlatformVerifier, Produced,
    Statement, Tier,
};
use crate::confidential::ConfidentialSpaceVerifier;
use crate::record::{hex, unhex_vec};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use p384::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};

/// The `platform` value in statements carrying only GPU evidence.
pub const GPU_KIND: &str = "nvidia-gpu";
/// The `platform` value in statements carrying Confidential Space and GPU evidence together.
pub const CPU_GPU_KIND: &str = "tdx+gpu";
pub const NRAS_ISSUER: &str = "https://nras.attestation.nvidia.com";

/// Claims in a per-GPU token that must be present and true. Any other `x-nvidia-gpu-*` boolean that is false also rejects.
const REQUIRED: [&str; 3] = [
    "x-nvidia-gpu-arch-check",
    "x-nvidia-gpu-attestation-report-signature-verified",
    "x-nvidia-gpu-attestation-report-nonce-match",
];

fn system_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct NvidiaGpuVerifier {
    keys: Vec<(String, VerifyingKey)>,
    issuer: String,
    drivers: BTreeSet<String>,
    vbios: BTreeSet<String>,
    models: BTreeSet<String>,
    min_gpus: usize,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl NvidiaGpuVerifier {
    #[must_use]
    pub fn new() -> Self {
        NvidiaGpuVerifier {
            keys: Vec::new(),
            issuer: NRAS_ISSUER.into(),
            drivers: BTreeSet::new(),
            vbios: BTreeSet::new(),
            // What NVIDIA reports for an H100 in CC mode; other Hopper/Blackwell names are added with `with_model`.
            models: ["GH100 A01 GSP BROM".to_string()].into(),
            min_gpus: 1,
            clock: Box::new(system_secs),
        }
    }

    /// Pin signing keys from a JWKS document (`{"keys":[{"kid","kty":"EC","crv":"P-384","x","y"}]}`).
    ///
    /// # Errors
    ///
    /// Returns an [`AttestError`] if the JWKS is malformed or holds no P-384 key.
    pub fn with_jwks(mut self, jwks: &str) -> Result<Self, AttestError> {
        let v: Value = serde_json::from_str(jwks).map_err(|e| format!("JWKS: {e}"))?;
        for k in v["keys"].as_array().ok_or("JWKS has no keys array")? {
            if k["kty"] != "EC" || k["crv"] != "P-384" {
                continue;
            }
            let kid = k["kid"].as_str().ok_or("JWKS key without kid")?.to_string();
            let coord = |name: &str| -> Result<Vec<u8>, AttestError> {
                B64.decode(
                    k[name]
                        .as_str()
                        .ok_or_else(|| format!("JWKS key without {name}"))?,
                )
                .map_err(|e| AttestError::from(e.to_string()))
            };
            let (x, y) = (coord("x")?, coord("y")?);
            if x.len() != 48 || y.len() != 48 {
                return Err("JWKS P-384 coordinates must be 48 bytes".into());
            }
            let mut point = vec![4u8];
            point.extend(x);
            point.extend(y);
            let key = VerifyingKey::from_sec1_bytes(&point).map_err(|e| e.to_string())?;
            self.keys.push((kid, key));
        }
        if self.keys.is_empty() {
            return Err("JWKS has no P-384 keys".into());
        }
        Ok(self)
    }

    #[must_use]
    pub fn with_key(mut self, kid: &str, key: VerifyingKey) -> Self {
        self.keys.push((kid.to_string(), key));
        self
    }

    /// Allow a driver version (`x-nvidia-gpu-driver-version`). With no driver and VBIOS pinned, evidence tops out at T1.
    #[must_use]
    pub fn with_driver(mut self, version: &str) -> Self {
        self.drivers.insert(version.to_string());
        self
    }

    /// Allow a VBIOS version (`x-nvidia-gpu-vbios-version`).
    #[must_use]
    pub fn with_vbios(mut self, version: &str) -> Self {
        self.vbios.insert(version.to_string());
        self
    }

    /// Accept another `hwmodel` string.
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        self.models.insert(model.to_string());
        self
    }

    /// How many GPUs the evidence must cover (default 1).
    #[must_use]
    pub fn with_min_gpus(mut self, n: usize) -> Self {
        self.min_gpus = n.max(1);
        self
    }

    #[must_use]
    pub fn with_issuer(mut self, issuer: &str) -> Self {
        self.issuer = issuer.into();
        self
    }

    /// Seconds since the epoch; tests set it so tokens do not expire under them.
    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// Verify one ES384 token against the pinned keys and return its claims.
    fn open(&self, token: &str) -> Result<Value, AttestError> {
        let parts: Vec<&str> = token.split('.').collect();
        let [h, p, s] = parts[..] else {
            return Err("token is not a three-part JWT".into());
        };
        let header: Value =
            serde_json::from_slice(&B64.decode(h).map_err(|e| format!("header: {e}"))?)
                .map_err(|e| format!("header: {e}"))?;
        // Only ES384, never `none` or a symmetric algorithm an attacker could pick.
        if header["alg"] != "ES384" {
            return Err(format!("unsupported alg {}", header["alg"]).into());
        }
        let kid = header["kid"].as_str().ok_or("token has no kid")?;
        let key = &self
            .keys
            .iter()
            .find(|(k, _)| k == kid)
            .ok_or_else(|| format!("no pinned key {kid:?}"))?
            .1;
        let raw = B64.decode(s).map_err(|e| format!("signature: {e}"))?;
        let sig = Signature::from_slice(&raw).map_err(|e| e.to_string())?;
        key.verify(format!("{h}.{p}").as_bytes(), &sig)
            .map_err(|_| "token signature does not verify")?;
        let c: Value = serde_json::from_slice(&B64.decode(p).map_err(|e| format!("claims: {e}"))?)
            .map_err(|e| format!("claims: {e}"))?;
        let now = (self.clock)();
        if c["iss"] != self.issuer.as_str() {
            return Err(format!("issuer is {}, expected {}", c["iss"], self.issuer).into());
        }
        if c["exp"].as_u64().ok_or("token has no exp")? <= now {
            return Err("token expired".into());
        }
        if c["nbf"].as_u64().is_some_and(|n| n > now + 60) {
            return Err("token not yet valid".into());
        }
        Ok(c)
    }

    /// `evidence` holds the `gpu` and `gpu-detail` lines; `binding_hex` is the statement binding.
    fn check(&self, evidence: &str, binding_hex: &str) -> Result<PlatformVerdict, AttestError> {
        let mut overall = None;
        let mut details: Vec<(String, String)> = Vec::new();
        for line in evidence.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(t) = line.strip_prefix("gpu-detail ") {
                let (name, jwt) = t
                    .trim()
                    .split_once(' ')
                    .ok_or("gpu-detail needs a name and a token")?;
                details.push((name.to_string(), jwt.trim().to_string()));
            } else if let Some(t) = line.strip_prefix("gpu ") {
                if overall.replace(t.trim().to_string()).is_some() {
                    return Err("more than one overall gpu token".into());
                }
            }
        }
        let overall = overall.ok_or("evidence has no gpu line")?;
        let c = self.open(&overall)?;
        let nonce_ok = match &c["eat_nonce"] {
            Value::String(n) => n.eq_ignore_ascii_case(binding_hex),
            Value::Array(a) => a.iter().any(|x| {
                x.as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(binding_hex))
            }),
            _ => false,
        };
        if !nonce_ok {
            return Err("token does not carry this statement's binding as its nonce".into());
        }
        if c["x-nvidia-overall-att-result"] != true {
            return Err("NVIDIA's overall attestation result is not true".into());
        }
        // `submods` lists each GPU with the SHA-256 of its detached token: ["DIGEST", ["SHA256", "<hex>"]].
        let submods = c["submods"]
            .as_object()
            .ok_or("overall token has no submods")?;
        let gpus: Vec<(&String, &str)> = submods
            .iter()
            .filter_map(|(name, v)| Some((name, v.get(1)?.get(1)?.as_str()?)))
            .collect();
        if gpus.len() < self.min_gpus {
            return Err(
                format!("{} GPU(s) attested, {} required", gpus.len(), self.min_gpus).into(),
            );
        }
        let mut drivers = BTreeSet::new();
        let mut biosv = BTreeSet::new();
        for (name, digest) in &gpus {
            let jwt = &details
                .iter()
                .find(|(n, _)| n == *name)
                .ok_or_else(|| format!("no detached token for {name}"))?
                .1;
            if !hex(&Sha256::digest(jwt.as_bytes())).eq_ignore_ascii_case(digest) {
                return Err(format!("detached token for {name} does not match its digest").into());
            }
            let d = self.open(jwt)?;
            if d["eat_nonce"]
                .as_str()
                .is_some_and(|n| !n.eq_ignore_ascii_case(binding_hex))
            {
                return Err(format!("{name}: nonce is not this statement's").into());
            }
            let model = d["hwmodel"].as_str().unwrap_or("");
            if !self.models.contains(model) {
                return Err(format!("{name}: hardware model {model:?} is not accepted").into());
            }
            if d["measres"] != "success" {
                return Err(format!(
                    "{name}: measurements do not match the reference ({})",
                    d["measres"]
                )
                .into());
            }
            if d["secboot"] != true {
                return Err(format!("{name}: secure boot is off").into());
            }
            if d["dbgstat"] != "disabled" {
                return Err(format!("{name}: debug is not disabled ({})", d["dbgstat"]).into());
            }
            for k in REQUIRED {
                if d[k] != true {
                    return Err(format!("{name}: {k} is not true").into());
                }
            }
            if let Some((k, _)) = d
                .as_object()
                .into_iter()
                .flatten()
                .find(|(k, v)| k.starts_with("x-nvidia-gpu-") && **v == false)
            {
                return Err(format!("{name}: {k} is false").into());
            }
            drivers.insert(
                d["x-nvidia-gpu-driver-version"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            );
            biosv.insert(
                d["x-nvidia-gpu-vbios-version"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            );
        }
        let pinned = !self.drivers.is_empty() && !self.vbios.is_empty();
        let mut notes = vec![format!(
            "{} NVIDIA GPU(s) attested in confidential-computing mode, debug disabled, secure boot on",
            gpus.len()
        )];
        let tier = if !pinned {
            notes.push(format!("driver {drivers:?} and VBIOS {biosv:?} are not pinned, so this is a genuine CC-mode GPU running unknown firmware"));
            Tier::T1
        } else if drivers.iter().all(|d| self.drivers.contains(d))
            && biosv.iter().all(|b| self.vbios.contains(b))
        {
            notes.push(format!(
                "driver {drivers:?} and VBIOS {biosv:?} are on the allow-lists"
            ));
            Tier::T3
        } else {
            return Err(
                format!("driver {drivers:?} or VBIOS {biosv:?} is not on the allow-list").into(),
            );
        };
        let ueid = c["ueid"].as_str().unwrap_or("");
        Ok(PlatformVerdict {
            tier,
            notes,
            challenge: None,
            device: (!ueid.is_empty()).then(|| crate::platform::fingerprint(ueid.as_bytes())),
        })
    }
}

impl Default for NvidiaGpuVerifier {
    fn default() -> Self {
        Self::new()
    }
}

fn evidence_text(statement: &Statement) -> Result<String, AttestError> {
    String::from_utf8(unhex_vec(&statement.platform_evidence)?)
        .map_err(|_| AttestError::from("evidence is not utf-8"))
}

impl PlatformVerifier for NvidiaGpuVerifier {
    fn kind(&self) -> &str {
        GPU_KIND
    }

    fn verify(&self, statement: &Statement) -> Result<PlatformVerdict, AttestError> {
        self.check(&evidence_text(statement)?, &hex(&statement.binding()))
            .map_err(|e| AttestError::Platform(e.to_string()))
    }
}

/// A Confidential Space CPU verifier and a GPU verifier over one piece of evidence, under one nonce. Both must
/// pass; the tier is the lower of the two.
pub struct CpuGpuVerifier {
    pub cpu: ConfidentialSpaceVerifier,
    pub gpu: NvidiaGpuVerifier,
}

impl PlatformVerifier for CpuGpuVerifier {
    fn kind(&self) -> &str {
        CPU_GPU_KIND
    }

    fn verify(&self, statement: &Statement) -> Result<PlatformVerdict, AttestError> {
        let text = evidence_text(statement)?;
        let binding = hex(&statement.binding());
        let cpu_token = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("token "))
            .ok_or_else(|| AttestError::Platform("evidence has no CPU token line".into()))?;
        let cpu = self
            .cpu
            .check(cpu_token.trim(), &binding)
            .map_err(|e| AttestError::Platform(format!("CPU: {e}")))?;
        let gpu = self
            .gpu
            .check(&text, &binding)
            .map_err(|e| AttestError::Platform(format!("GPU: {e}")))?;
        let mut notes = cpu.notes;
        notes.extend(gpu.notes);
        Ok(PlatformVerdict {
            tier: cpu.tier.min(gpu.tier),
            notes,
            challenge: None,
            device: cpu.device,
        })
    }
}

/// Produces `nvidia-gpu` evidence alone: the lines a command prints for the nonce in `PLAYBOX_NONCE`. For machines with no
/// confidential CPU (or for testing with `examples/fake_nras.rs`); [`CpuGpuProvider`] is the combined form.
#[cfg(unix)]
pub struct GpuProvider {
    pub gpu_command: std::path::PathBuf,
}

#[cfg(unix)]
fn run_gpu_command(cmd: &std::path::Path, binding: &[u8; 32]) -> Result<String, AttestError> {
    let out = std::process::Command::new(cmd)
        .env("PLAYBOX_NONCE", hex(binding))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("running {}: {e}", cmd.display()))?;
    if !out.status.success() {
        return Err(format!("{} failed: {}", cmd.display(), out.status).into());
    }
    Ok(String::from_utf8(out.stdout).map_err(|_| "GPU evidence is not utf-8")?)
}

#[cfg(unix)]
impl PlatformProvider for GpuProvider {
    fn kind(&self) -> &str {
        GPU_KIND
    }

    fn produce(&self, binding: &[u8; 32], _: &Measurements) -> Result<Produced, AttestError> {
        let gpu = run_gpu_command(&self.gpu_command, binding)?;
        Ok(Produced {
            evidence: format!("{}\n", gpu.trim_end()),
            claimed: Tier::T3,
        })
    }
}

/// Produces the evidence for [`CpuGpuVerifier`]: the Confidential Space token from the launcher, plus the lines a
/// command prints for the GPU. The command gets the nonce in `PLAYBOX_NONCE` and must print the `gpu` and
/// `gpu-detail` lines (a small wrapper around `nvtrust` or NRAS). Only works inside a CC-mode VM with an H100,
/// and has not been run on one.
#[cfg(unix)]
pub struct CpuGpuProvider {
    pub cpu: crate::confidential::ConfidentialSpaceProvider,
    pub gpu_command: std::path::PathBuf,
}

#[cfg(unix)]
impl PlatformProvider for CpuGpuProvider {
    fn kind(&self) -> &str {
        CPU_GPU_KIND
    }

    fn produce(&self, binding: &[u8; 32], m: &Measurements) -> Result<Produced, AttestError> {
        let cpu = self.cpu.produce(binding, m)?;
        let gpu = run_gpu_command(&self.gpu_command, binding)?;
        Ok(Produced {
            evidence: format!("{}\n{}\n", cpu.evidence.trim_end(), gpu.trim_end()),
            claimed: Tier::T3,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attest::{new_statement, verify};
    use ed25519_dalek::SigningKey;
    use p384::ecdsa::{signature::Signer, SigningKey as P384Signer};
    use serde_json::json;

    const NOW: u64 = 1_800_000_000;

    fn nvidia() -> P384Signer {
        P384Signer::from_slice(&[7u8; 48]).unwrap()
    }

    fn meas() -> Measurements {
        Measurements {
            binary: "bb".into(),
            runner: "libkrun".into(),
            policy: "pp".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    fn jwt(kid: &str, alg: &str, claims: &Value, key: &P384Signer) -> String {
        let h = B64.encode(json!({"alg": alg, "kid": kid}).to_string());
        let p = B64.encode(claims.to_string());
        let sig: Signature = key.sign(format!("{h}.{p}").as_bytes());
        format!("{h}.{p}.{}", B64.encode(sig.to_bytes()))
    }

    fn detail_claims(binding: &str) -> Value {
        json!({
            "iss": NRAS_ISSUER, "exp": NOW + 600, "eat_nonce": binding,
            "hwmodel": "GH100 A01 GSP BROM", "measres": "success", "secboot": true, "dbgstat": "disabled",
            "x-nvidia-gpu-arch-check": true,
            "x-nvidia-gpu-attestation-report-signature-verified": true,
            "x-nvidia-gpu-attestation-report-nonce-match": true,
            "x-nvidia-gpu-attestation-report-cert-chain-validated": true,
            "x-nvidia-gpu-driver-version": "550.90.07", "x-nvidia-gpu-vbios-version": "96.00.74.00.1A"
        })
    }

    /// The evidence for a statement binding: an overall token over one detached token per GPU.
    fn evidence_with(binding: &str, tweak: impl Fn(&mut Value), gpus: usize) -> String {
        let key = nvidia();
        let mut lines = Vec::new();
        let mut submods = serde_json::Map::new();
        for i in 0..gpus {
            let mut d = detail_claims(binding);
            tweak(&mut d);
            let t = jwt("n1", "ES384", &d, &key);
            let name = format!("GPU-{i}");
            submods.insert(
                name.clone(),
                json!(["DIGEST", ["SHA256", hex(&Sha256::digest(t.as_bytes()))]]),
            );
            lines.push(format!("gpu-detail {name} {t}"));
        }
        let overall = json!({
            "iss": NRAS_ISSUER, "exp": NOW + 600, "eat_nonce": binding, "ueid": "1234567890",
            "x-nvidia-overall-att-result": true, "submods": submods
        });
        format!(
            "gpu {}\n{}\n",
            jwt("n1", "ES384", &overall, &key),
            lines.join("\n")
        )
    }

    fn verifier() -> NvidiaGpuVerifier {
        NvidiaGpuVerifier::new()
            .with_key("n1", *nvidia().verifying_key())
            .with_clock(|| NOW)
    }

    fn pinned() -> NvidiaGpuVerifier {
        verifier()
            .with_driver("550.90.07")
            .with_vbios("96.00.74.00.1A")
    }

    fn statement(evidence_for: impl Fn(&str) -> String) -> String {
        let key = SigningKey::from_bytes(&[5; 32]);
        let mut st = new_statement(&key, "ab", "", 1, meas());
        st.platform = GPU_KIND.into();
        st.claimed = Tier::T3;
        st.platform_evidence = hex(evidence_for(&hex(&st.binding())).as_bytes());
        st.sign(&key).unwrap()
    }

    fn achieved(v: &NvidiaGpuVerifier, text: &str) -> (Tier, String) {
        let r = verify(text, &[v]).unwrap();
        (r.achieved, r.notes.join(" | "))
    }

    #[test]
    fn pinned_firmware_on_a_genuine_gpu_reaches_t3() {
        let (tier, notes) = achieved(&pinned(), &statement(|b| evidence_with(b, |_| {}, 1)));
        assert_eq!(tier, Tier::T3, "{notes}");
    }

    #[test]
    fn unpinned_firmware_only_reaches_t1() {
        let (tier, notes) = achieved(&verifier(), &statement(|b| evidence_with(b, |_| {}, 1)));
        assert_eq!(tier, Tier::T1, "{notes}");
    }

    #[test]
    fn each_failed_check_leaves_the_statement_at_t0() {
        type Tweak = Box<dyn Fn(&mut Value)>;
        let bad: Vec<(&str, Tweak)> = vec![
            ("measurements", Box::new(|d| d["measres"] = json!("fail"))),
            ("debug on", Box::new(|d| d["dbgstat"] = json!("enabled"))),
            ("secure boot", Box::new(|d| d["secboot"] = json!(false))),
            ("wrong GPU", Box::new(|d| d["hwmodel"] = json!("TU104"))),
            (
                "nonce check",
                Box::new(|d| d["x-nvidia-gpu-attestation-report-nonce-match"] = json!(false)),
            ),
            (
                "missing check",
                Box::new(|d| {
                    d.as_object_mut().unwrap().remove("x-nvidia-gpu-arch-check");
                }),
            ),
            (
                "extra false",
                Box::new(|d| d["x-nvidia-gpu-driver-rim-signature-verified"] = json!(false)),
            ),
            (
                "old driver",
                Box::new(|d| d["x-nvidia-gpu-driver-version"] = json!("535.0")),
            ),
            ("other nonce", Box::new(|d| d["eat_nonce"] = json!("00"))),
        ];
        for (what, tweak) in bad {
            let text = statement(|b| evidence_with(b, &tweak, 1));
            let (tier, notes) = achieved(&pinned(), &text);
            assert_eq!(tier, Tier::T0, "{what}: {notes}");
        }
    }

    #[test]
    fn a_token_for_another_statement_is_rejected() {
        let other = evidence_with(&"ab".repeat(32), |_| {}, 1);
        let (tier, notes) = achieved(&verifier(), &statement(|_| other.clone()));
        assert_eq!(tier, Tier::T0, "{notes}");
    }

    #[test]
    fn too_few_gpus_forged_signatures_and_weak_algorithms_are_rejected() {
        let two = verifier().with_min_gpus(2);
        assert_eq!(
            achieved(&two, &statement(|b| evidence_with(b, |_| {}, 1))).0,
            Tier::T0
        );
        assert_eq!(
            achieved(&two, &statement(|b| evidence_with(b, |_| {}, 2))).0,
            Tier::T1
        );
        // Signed by a key nobody pinned.
        let forger = P384Signer::from_slice(&[9u8; 48]).unwrap();
        let overall = |b: &str| {
            json!({"iss": NRAS_ISSUER, "exp": NOW + 600, "eat_nonce": b,
                "x-nvidia-overall-att-result": true, "submods": {}})
        };
        let forged = statement(|b| format!("gpu {}\n", jwt("n1", "ES384", &overall(b), &forger)));
        assert_eq!(achieved(&verifier(), &forged).0, Tier::T0);
        // `alg: none` and swapped algorithm labels.
        for alg in ["none", "HS384", "RS256"] {
            let t = statement(|b| format!("gpu {}\n", jwt("n1", alg, &overall(b), &nvidia())));
            assert_eq!(achieved(&verifier(), &t).0, Tier::T0, "{alg}");
        }
        // A detached token swapped for another one no longer matches its digest.
        let swapped = statement(|b| {
            let good = evidence_with(b, |_| {}, 1);
            let other = evidence_with(b, |d| d["x-nvidia-gpu-driver-version"] = json!("999"), 1);
            let detail = other
                .lines()
                .find(|l| l.starts_with("gpu-detail"))
                .unwrap()
                .to_string();
            format!("{}\n{detail}\n", good.lines().next().unwrap())
        });
        assert_eq!(achieved(&verifier(), &swapped).0, Tier::T0);
        // Expired.
        let late = NvidiaGpuVerifier::new()
            .with_key("n1", *nvidia().verifying_key())
            .with_clock(|| NOW + 10_000);
        assert_eq!(
            achieved(&late, &statement(|b| evidence_with(b, |_| {}, 1))).0,
            Tier::T0
        );
    }

    #[test]
    fn jwks_loading_pins_p384_keys() {
        let pt = nvidia().verifying_key().to_sec1_point(false);
        let jwks = json!({"keys": [
            {"kid": "n1", "kty": "EC", "crv": "P-384", "x": B64.encode(pt.x().unwrap()), "y": B64.encode(pt.y().unwrap())},
            {"kid": "r", "kty": "RSA", "n": "AQAB", "e": "AQAB"}
        ]})
        .to_string();
        let v = NvidiaGpuVerifier::new()
            .with_jwks(&jwks)
            .unwrap()
            .with_clock(|| NOW);
        assert_eq!(
            achieved(&v, &statement(|b| evidence_with(b, |_| {}, 1))).0,
            Tier::T1
        );
        assert!(NvidiaGpuVerifier::new()
            .with_jwks(r#"{"keys":[]}"#)
            .is_err());
        assert!(NvidiaGpuVerifier::new().with_jwks("nope").is_err());
    }

    /// CPU and GPU evidence under one nonce: both must pass, and the tier is the lower of the two.
    #[test]
    fn cpu_and_gpu_evidence_together_take_the_lower_tier() {
        use crate::confidential::GOOGLE_ISSUER;
        use rsa::{
            pkcs1v15::SigningKey as RsaSigner,
            rand_core::OsRng,
            signature::{SignatureEncoding, Signer as _},
            RsaPrivateKey,
        };
        let google = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let cs_token = |binding: &str| {
            let claims = json!({
                "iss": GOOGLE_ISSUER, "aud": "playbox", "exp": NOW + 600, "nbf": NOW - 5, "eat_nonce": [binding],
                "hwmodel": "INTEL_TDX", "swname": "CONFIDENTIAL_SPACE", "dbgstat": "disabled-since-boot", "secboot": true,
                "submods": {
                    "confidential_space": {"support_attributes": ["STABLE"]},
                    "container": {"image_digest": "sha256:aaaa"},
                    "gce": {"project_id": "proj", "instance_id": "123"}
                }
            });
            let h = B64.encode(json!({"alg": "RS256", "kid": "g1"}).to_string());
            let p = B64.encode(claims.to_string());
            let sig = RsaSigner::<Sha256>::new(google.clone()).sign(format!("{h}.{p}").as_bytes());
            format!("{h}.{p}.{}", B64.encode(sig.to_bytes()))
        };
        let both = |cpu_pinned: bool, gpu: NvidiaGpuVerifier| {
            let mut cpu = ConfidentialSpaceVerifier::new("playbox")
                .with_key("g1", google.to_public_key())
                .with_clock(|| NOW);
            if cpu_pinned {
                cpu = cpu.with_image_digest("sha256:aaaa");
            }
            CpuGpuVerifier { cpu, gpu }
        };
        let key = SigningKey::from_bytes(&[5; 32]);
        let mut st = new_statement(&key, "ab", "", 1, meas());
        st.platform = CPU_GPU_KIND.into();
        st.claimed = Tier::T3;
        let b = hex(&st.binding());
        let ev = format!("token {}\n{}", cs_token(&b), evidence_with(&b, |_| {}, 1));
        st.platform_evidence = hex(ev.as_bytes());
        let text = st.sign(&key).unwrap();
        let tier = |v: &CpuGpuVerifier| verify(&text, &[v]).unwrap().achieved;
        assert_eq!(tier(&both(true, pinned())), Tier::T3);
        assert_eq!(
            tier(&both(false, pinned())),
            Tier::T1,
            "CPU unpinned caps it"
        );
        assert_eq!(
            tier(&both(true, verifier())),
            Tier::T1,
            "GPU unpinned caps it"
        );
        // Evidence without the GPU half is not enough.
        let mut cpu_only = Statement::parse(&text).unwrap();
        cpu_only.platform_evidence = hex(format!("token {}\n", cs_token(&b)).as_bytes());
        let cpu_only = cpu_only.sign(&key).unwrap();
        assert_eq!(
            verify(&cpu_only, &[&both(true, pinned())])
                .unwrap()
                .achieved,
            Tier::T0
        );
    }
}
