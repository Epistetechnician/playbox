//! Platform evidence for [`crate::attest`]: device-bound keys on macOS (Secure Enclave) and Linux (TPM 2.0).
//!
//! Verifiers here are pure Rust and run on any OS, so a Mac can verify a Linux host's TPM quote and
//! the reverse. Providers talk to the hardware and only work where it exists.
//!
//! # What this does and does not prove
//!
//! Both kinds of evidence show that *a key held by a device signed this statement's binding*
//! (see [`crate::attest::Statement::binding`]). Neither proves, on its own, that the device is genuine
//! hardware: a software TPM or a plain P-256 key produces the same bytes. So a verifier only reaches
//! [`Tier::T1`] when the device key is **pinned** in its [`PinStore`], meaning an operator decided to
//! trust that key as that device. Without a pin the evidence is reported (with the key's fingerprint,
//! so it can be pinned) and the statement stays at T0.
//!
//! [`Tier::T2`] additionally needs a TPM quote showing the host's declared measurements were extended
//! into PCR 23, *and* operator-supplied reference values for at least one boot PCR (0-7) that match.
//! Without reference boot values the quote could not distinguish an honest host from one that simply
//! extended whatever it liked.
//!
//! TPM endorsement-key certificate chains and credential activation (`$PLAYBOX_TPM_EK_ANCHORS`; see
//! [`crate::tpm_ek`]) verify genuine TPM hardware when configured. Not implemented: Apple's App Attest
//! for Secure Enclave keys, which rely on device-key pinning alone.

use crate::attest::{AttestError, PlatformProvider, PlatformVerifier, Statement};
use crate::tpm_ek;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

mod pins;
mod secure_enclave;
mod tpm_provider;
mod tpm_verify;
pub use pins::PinStore;
pub use secure_enclave::{se_message, SecureEnclaveProvider, SecureEnclaveVerifier, SE_KIND};
pub use tpm_provider::TpmProvider;
pub use tpm_verify::{
    extended_from_zero, measurement_digest, TpmVerifier, MEASUREMENT_PCR, QUOTED_PCRS, TPM_KIND,
};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>, AttestError> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err("odd-length or non-ascii hex".into());
    }
    (0..s.len() / 2)
        .map(|i| {
            u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|e| AttestError::Malformed(e.to_string()))
        })
        .collect()
}

/// Stable identifier of a device key: hex blake3 of its public key bytes.
#[must_use]
pub fn fingerprint(public_key: &[u8]) -> String {
    hex(blake3::hash(public_key).as_bytes())
}

/// Evidence is `key value` lines, hex-encoded inside the statement.
fn parse_evidence(statement: &Statement) -> Result<BTreeMap<String, String>, AttestError> {
    let raw = unhex(&statement.platform_evidence)?;
    let text = String::from_utf8(raw).map_err(|_| "evidence is not utf-8")?;
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let (k, v) = line
            .split_once(' ')
            .ok_or_else(|| format!("bad evidence line {line:?}"))?;
        if m.insert(k.to_string(), v.to_string()).is_some() {
            return Err(format!("duplicate evidence key {k}").into());
        }
    }
    Ok(m)
}

fn need<'a>(m: &'a BTreeMap<String, String>, k: &str) -> Result<&'a str, AttestError> {
    m.get(k)
        .map(String::as_str)
        .ok_or_else(|| AttestError::Malformed(format!("evidence is missing `{k}`")))
}

// ----------------------------------------------------------------------------------------------
// Pins
// ----------------------------------------------------------------------------------------------

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(name))
            .find(|f| f.is_file())
    })
}

/// The `playbox-se` helper: `$PLAYBOX_SE_HELPER`, next to this executable, or on `PATH`.
#[must_use]
pub fn find_se_helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PLAYBOX_SE_HELPER") {
        return Some(PathBuf::from(p));
    }
    if let Some(next) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("playbox-se")))
    {
        if next.is_file() {
            return Some(next);
        }
    }
    which("playbox-se")
}

/// Where trusted device keys are listed: `<home>/pins`.
#[must_use]
pub fn pins_path(home: &Path) -> PathBuf {
    home.join("pins")
}

/// Boot PCR reference values from `$PLAYBOX_TPM_BOOT_PCRS`, e.g. `0:<hex>,7:<hex>`. Empty if unset.
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
pub fn boot_reference_from_env() -> Result<BTreeMap<u32, String>, AttestError> {
    let Ok(v) = std::env::var("PLAYBOX_TPM_BOOT_PCRS") else {
        return Ok(BTreeMap::new());
    };
    let mut m = BTreeMap::new();
    for e in v.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (i, h) = e
            .split_once(':')
            .ok_or_else(|| format!("PLAYBOX_TPM_BOOT_PCRS entry {e:?} must be index:hex"))?;
        m.insert(
            i.parse().map_err(|_| format!("bad PCR index {i:?}"))?,
            h.to_string(),
        );
    }
    Ok(m)
}

/// Pick a provider from `$PLAYBOX_ATTEST` (see [`detect_with`]).
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
pub fn detect(home: &Path) -> Result<Option<Box<dyn PlatformProvider>>, AttestError> {
    detect_with(
        home,
        &std::env::var("PLAYBOX_ATTEST").unwrap_or_else(|_| "auto".into()),
    )
}

/// Pick a provider by `mode`: `none`, `auto`, `se`, `tpm` or `tdx` (Confidential Space; never chosen by `auto`).
/// `auto` uses the Secure Enclave on macOS if the helper is present, and a TPM on Linux if `tpm2-tools`
/// and a TPM (or `TPM2TOOLS_TCTI`) are present. `se` and `tpm` fail loudly if unavailable.
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
pub fn detect_with(
    home: &Path,
    mode: &str,
) -> Result<Option<Box<dyn PlatformProvider>>, AttestError> {
    let se = || -> Result<Box<dyn PlatformProvider>, AttestError> {
        let helper = find_se_helper()
            .ok_or("playbox-se helper not found (build it with tools/playbox-se/build.sh)")?;
        Ok(Box::new(SecureEnclaveProvider {
            helper,
            key_file: home.join("se.key"),
        }))
    };
    let tpm = || -> Result<Box<dyn PlatformProvider>, AttestError> {
        if which("tpm2_quote").is_none() {
            return Err("tpm2-tools not found".into());
        }
        Ok(Box::new(TpmProvider::default()))
    };
    match mode {
        "none" => Ok(None),
        "se" => se().map(Some),
        "tpm" => tpm().map(Some),
        #[cfg(unix)]
        "tdx" => {
            let audience = std::env::var("PLAYBOX_CS_AUDIENCE").map_err(|_| {
                "PLAYBOX_ATTEST=tdx needs PLAYBOX_CS_AUDIENCE (the audience your verifiers expect)"
            })?;
            Ok(Some(Box::new(
                crate::confidential::ConfidentialSpaceProvider::new(&audience),
            )))
        }
        #[cfg(unix)]
        "gpu" => {
            let cmd = std::env::var_os("PLAYBOX_GPU_EVIDENCE_CMD").ok_or(
                "PLAYBOX_ATTEST=gpu needs PLAYBOX_GPU_EVIDENCE_CMD (a program that prints the gpu and gpu-detail evidence lines for the nonce in PLAYBOX_NONCE)",
            )?;
            Ok(Some(Box::new(crate::gpu_cc::GpuProvider {
                gpu_command: cmd.into(),
            })))
        }
        #[cfg(unix)]
        "tdx+gpu" => {
            let audience = std::env::var("PLAYBOX_CS_AUDIENCE").map_err(|_| {
                "PLAYBOX_ATTEST=tdx+gpu needs PLAYBOX_CS_AUDIENCE (the audience your verifiers expect)"
            })?;
            let cmd = std::env::var_os("PLAYBOX_GPU_EVIDENCE_CMD").ok_or(
                "PLAYBOX_ATTEST=tdx+gpu needs PLAYBOX_GPU_EVIDENCE_CMD (a program that prints the gpu and gpu-detail evidence lines for the nonce in PLAYBOX_NONCE)",
            )?;
            Ok(Some(Box::new(crate::gpu_cc::CpuGpuProvider {
                cpu: crate::confidential::ConfidentialSpaceProvider::new(&audience),
                gpu_command: cmd.into(),
            })))
        }
        "auto" => Ok(if cfg!(target_os = "macos") {
            se().ok()
        } else if std::env::var_os("TPM2TOOLS_TCTI").is_some() || Path::new("/dev/tpmrm0").exists()
        {
            tpm().ok()
        } else {
            None
        }),
        other => Err(format!(
            "PLAYBOX_ATTEST must be none, auto, se, tpm, tdx, gpu or tdx+gpu, not {other:?}"
        )
        .into()),
    }
}

/// TPM manufacturer trust anchors from `$PLAYBOX_TPM_EK_ANCHORS`, a PEM file of the root or intermediate
/// certificates you trust to vouch for TPM endorsement keys. Empty if unset, which turns the genuineness
/// check off (device keys are then trusted by pin alone).
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
pub fn ek_anchors_from_env() -> Result<Vec<Vec<u8>>, AttestError> {
    let Some(path) = std::env::var_os("PLAYBOX_TPM_EK_ANCHORS") else {
        return Ok(Vec::new());
    };
    let text =
        fs::read_to_string(&path).map_err(|e| format!("reading PLAYBOX_TPM_EK_ANCHORS: {e}"))?;
    tpm_ek::pem_to_ders(&text)
        .map_err(|e| AttestError::Malformed(format!("PLAYBOX_TPM_EK_ANCHORS: {e}")))
}

/// Verifiers for every platform kind, trusting the keys in `pins`. TPM genuineness checks are on when
/// `$PLAYBOX_TPM_EK_ANCHORS` is set.
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
pub fn verifiers(
    pins: PinStore,
    boot_reference: BTreeMap<u32, String>,
) -> Result<Vec<Box<dyn PlatformVerifier>>, AttestError> {
    let mut v: Vec<Box<dyn PlatformVerifier>> = vec![
        Box::new(SecureEnclaveVerifier::new(pins.clone())),
        Box::new(
            TpmVerifier::new(pins)
                .with_boot_reference(boot_reference)?
                .with_ek_anchors(ek_anchors_from_env()?),
        ),
    ];
    if let Some(gpu) = nvidia_gpu_from_env()? {
        if let Some(cs) = confidential_space_from_env()? {
            v.push(Box::new(crate::gpu_cc::CpuGpuVerifier { cpu: cs, gpu }));
        }
        if let Some(gpu) = nvidia_gpu_from_env()? {
            v.push(Box::new(gpu));
        }
    }
    if let Some(cs) = confidential_space_from_env()? {
        v.push(Box::new(cs));
    }
    Ok(v)
}

/// An NVIDIA GPU (`nvidia-gpu`) verifier from the environment, or `None` if unconfigured: `$PLAYBOX_GPU_JWKS` (file with
/// NVIDIA's attestation signing keys), `$PLAYBOX_GPU_DRIVERS` and `$PLAYBOX_GPU_VBIOS` (comma-separated allowed versions;
/// without both, evidence tops out at T1) and `$PLAYBOX_GPU_COUNT` (GPUs that must be attested, default 1). With
/// `$PLAYBOX_CS_JWKS` and `$PLAYBOX_CS_AUDIENCE` also set, the combined `tdx+gpu` kind is verified too.
///
/// # Errors
///
/// Returns an [`AttestError`] if the keys file cannot be read or holds no P-384 key.
pub fn nvidia_gpu_from_env() -> Result<Option<crate::gpu_cc::NvidiaGpuVerifier>, AttestError> {
    let Some(jwks) = std::env::var_os("PLAYBOX_GPU_JWKS") else {
        return Ok(None);
    };
    let text = fs::read_to_string(&jwks).map_err(|e| format!("reading PLAYBOX_GPU_JWKS: {e}"))?;
    let mut v = crate::gpu_cc::NvidiaGpuVerifier::new().with_jwks(&text)?;
    let list = |k: &str| std::env::var(k).unwrap_or_default();
    for d in list("PLAYBOX_GPU_DRIVERS")
        .split(',')
        .filter(|d| !d.is_empty())
    {
        v = v.with_driver(d.trim());
    }
    for b in list("PLAYBOX_GPU_VBIOS")
        .split(',')
        .filter(|b| !b.is_empty())
    {
        v = v.with_vbios(b.trim());
    }
    if let Ok(n) = list("PLAYBOX_GPU_COUNT").trim().parse::<usize>() {
        v = v.with_min_gpus(n);
    }
    Ok(Some(v))
}

/// A Confidential Space (`tdx`) verifier from the environment, or `None` if unconfigured:
/// `$PLAYBOX_CS_JWKS` (file with Google's attestation signing keys), `$PLAYBOX_CS_AUDIENCE`,
/// `$PLAYBOX_CS_IMAGES` (comma-separated image digests; without any, evidence tops out at T1) and
/// optionally `$PLAYBOX_CS_PROJECTS`.
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
pub fn confidential_space_from_env(
) -> Result<Option<crate::confidential::ConfidentialSpaceVerifier>, AttestError> {
    let (Some(jwks), Ok(audience)) = (
        std::env::var_os("PLAYBOX_CS_JWKS"),
        std::env::var("PLAYBOX_CS_AUDIENCE"),
    ) else {
        return Ok(None);
    };
    let text = fs::read_to_string(&jwks).map_err(|e| format!("reading PLAYBOX_CS_JWKS: {e}"))?;
    let mut v = crate::confidential::ConfidentialSpaceVerifier::new(&audience).with_jwks(&text)?;
    for d in std::env::var("PLAYBOX_CS_IMAGES")
        .unwrap_or_default()
        .split(',')
        .filter(|d| !d.is_empty())
    {
        v = v.with_image_digest(d.trim());
    }
    for p in std::env::var("PLAYBOX_CS_PROJECTS")
        .unwrap_or_default()
        .split(',')
        .filter(|p| !p.is_empty())
    {
        v = v.with_project(p.trim());
    }
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::tpm_verify::{
        parse_quote, TPM_ALG_ECDSA, TPM_ALG_SHA256, TPM_GENERATED, TPM_ST_ATTEST_QUOTE,
    };
    use super::*;
    use crate::attest::{self, new_statement, verify};
    use crate::attest::{Measurements, Produced, Tier};
    use ed25519_dalek::SigningKey;
    use p256::ecdsa::Signature;
    use p256::ecdsa::{signature::Signer, SigningKey as P256Signing};
    use sha2::{Digest, Sha256};

    fn session() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn meas() -> Measurements {
        Measurements {
            binary: "bb".into(),
            runner: "docker".into(),
            policy: "pp".into(),
            os: "linux".into(),
            arch: "aarch64".into(),
        }
    }

    fn device(n: u8) -> P256Signing {
        P256Signing::from_slice(&[n; 32]).unwrap()
    }

    fn point(k: &P256Signing) -> Vec<u8> {
        k.verifying_key().to_sec1_point(false).as_bytes().to_vec()
    }

    struct SoftSe(P256Signing);
    impl PlatformProvider for SoftSe {
        fn kind(&self) -> &str {
            SE_KIND
        }
        fn produce(&self, binding: &[u8; 32], _: &Measurements) -> Result<Produced, AttestError> {
            let sig: Signature = self.0.sign(&se_message(binding));
            Ok(Produced {
                evidence: format!(
                    "pub {}\nsig {}\n",
                    hex(&point(&self.0)),
                    hex(&sig.to_bytes())
                ),
                claimed: Tier::T1,
            })
        }
    }

    /// A TPM stand-in: builds real `TPMS_ATTEST` / `TPMT_SIGNATURE` bytes and signs them with a P-256 key.
    struct SoftTpm {
        ak: P256Signing,
        pcrs: BTreeMap<u32, [u8; 32]>,
        extra_override: Option<Vec<u8>>,
        ak_attributes: u32,
    }

    impl SoftTpm {
        fn akpub(&self) -> Vec<u8> {
            let p = point(&self.ak);
            tpm_ek::encode_ak(
                p[1..33].try_into().unwrap(),
                p[33..65].try_into().unwrap(),
                self.ak_attributes,
            )
        }

        #[allow(
            clippy::cast_possible_truncation,
            reason = "the value is bounded, or truncated on purpose"
        )]
        fn new(extend_measurements: Option<&Measurements>) -> Self {
            let mut pcrs: BTreeMap<u32, [u8; 32]> = QUOTED_PCRS
                .iter()
                .map(|i| (*i, [*i as u8 + 1; 32]))
                .collect();
            pcrs.insert(
                MEASUREMENT_PCR,
                extend_measurements.map_or([0; 32], |m| extended_from_zero(&measurement_digest(m))),
            );
            Self {
                ak: device(3),
                pcrs,
                extra_override: None,
                ak_attributes: tpm_ek::AK_ATTRIBUTES,
            }
        }
    }

    fn tpm2b(b: &[u8]) -> Vec<u8> {
        [
            &u16::try_from(b.len())
                .expect("length fits in u16")
                .to_be_bytes()[..],
            b,
        ]
        .concat()
    }

    impl PlatformProvider for SoftTpm {
        fn kind(&self) -> &str {
            TPM_KIND
        }
        fn produce(&self, binding: &[u8; 32], _: &Measurements) -> Result<Produced, AttestError> {
            let mut digest = Sha256::new();
            for v in self.pcrs.values() {
                digest.update(v);
            }
            let mut select = [0u8; 3];
            for i in self.pcrs.keys() {
                select[(*i / 8) as usize] |= 1 << (i % 8);
            }
            let mut q = Vec::new();
            q.extend(TPM_GENERATED.to_be_bytes());
            q.extend(TPM_ST_ATTEST_QUOTE.to_be_bytes());
            q.extend(tpm2b(b"signer-name"));
            q.extend(tpm2b(self.extra_override.as_deref().unwrap_or(binding)));
            q.extend(1234u64.to_be_bytes());
            q.extend(2u32.to_be_bytes());
            q.extend(3u32.to_be_bytes());
            q.push(1);
            q.extend(99u64.to_be_bytes());
            q.extend(1u32.to_be_bytes());
            q.extend(TPM_ALG_SHA256.to_be_bytes());
            q.push(3);
            q.extend(select);
            q.extend(tpm2b(&digest.finalize()));
            let sig: Signature = self.ak.sign(&q);
            let (r, s) = sig.split_bytes();
            let mut tsig = Vec::new();
            tsig.extend(TPM_ALG_ECDSA.to_be_bytes());
            tsig.extend(TPM_ALG_SHA256.to_be_bytes());
            tsig.extend(tpm2b(&r));
            tsig.extend(tpm2b(&s));
            let pcrs: Vec<String> = self
                .pcrs
                .iter()
                .map(|(i, v)| format!("{i}:{}", hex(v)))
                .collect();
            Ok(Produced {
                evidence: format!(
                    "akpub {}\nquote {}\nsig {}\npcrs {}\n",
                    hex(&self.akpub()),
                    hex(&q),
                    hex(&tsig),
                    pcrs.join(",")
                ),
                claimed: Tier::T2,
            })
        }
    }

    fn statement(p: &dyn PlatformProvider, nonce: &str) -> String {
        attest::new_platform_statement(&session(), nonce, "", 1, meas(), p).unwrap()
    }

    fn pinned(kind: &str, pubkey: &[u8]) -> PinStore {
        let mut p = PinStore::in_memory();
        p.add(kind, &fingerprint(pubkey)).unwrap();
        p
    }

    #[test]
    fn secure_enclave_evidence_needs_a_pin_for_t1() {
        let dev = device(5);
        let text = statement(&SoftSe(dev.clone()), "aa");
        let none = SecureEnclaveVerifier::new(PinStore::in_memory());
        let v = verify(&text, &[&none]).unwrap();
        assert_eq!(v.achieved, Tier::T0);
        assert!(v.notes.iter().any(|n| n.contains("not pinned")));
        assert_eq!(
            v.device.as_deref(),
            Some(format!("secure-enclave:{}", fingerprint(&point(&dev))).as_str())
        );

        let trusting = SecureEnclaveVerifier::new(pinned(SE_KIND, &point(&dev)));
        assert_eq!(verify(&text, &[&trusting]).unwrap().achieved, Tier::T1);
        // Pinning a different key does not help.
        let other = SecureEnclaveVerifier::new(pinned(SE_KIND, &point(&device(6))));
        assert_eq!(verify(&text, &[&other]).unwrap().achieved, Tier::T0);
    }

    #[test]
    fn secure_enclave_evidence_is_bound_to_its_statement() {
        let dev = device(5);
        let v = SecureEnclaveVerifier::new(pinned(SE_KIND, &point(&dev)));
        let a = statement(&SoftSe(dev.clone()), "aa");
        // Evidence lifted from one statement into another (different nonce) must not verify.
        let ev = Statement::parse(&a).unwrap().platform_evidence;
        let mut other = new_statement(&session(), "bb", "", 1, meas());
        other.platform = SE_KIND.into();
        other.claimed = Tier::T1;
        other.platform_evidence = ev;
        let forged = other.sign(&session()).unwrap();
        assert_eq!(verify(&forged, &[&v]).unwrap().achieved, Tier::T0);
        assert!(verify(&forged, &[&v])
            .unwrap()
            .notes
            .iter()
            .any(|n| n.contains("rejected") && n.contains("does not match")));
    }

    #[test]
    fn tpm_quote_verifies_and_tiers() {
        let tpm = SoftTpm::new(Some(&meas()));
        let ak = point(&tpm.ak)[1..].to_vec();
        let text = statement(&tpm, "0102");

        // Unpinned: valid quote, but not trusted.
        assert_eq!(
            verify(&text, &[&TpmVerifier::new(PinStore::in_memory())])
                .unwrap()
                .achieved,
            Tier::T0
        );
        // Pinned, no boot reference: T1 only, even though PCR 23 matches.
        let pinned_v = TpmVerifier::new(pinned(TPM_KIND, &ak));
        let v = verify(&text, &[&pinned_v]).unwrap();
        assert_eq!(v.achieved, Tier::T1);
        assert!(v.notes.iter().any(|n| n.contains("no boot reference")));
        // Pinned with matching boot reference: T2.
        let refs = BTreeMap::from([(0u32, hex(&[1u8; 32])), (7u32, hex(&[8u8; 32]))]);
        let strict = TpmVerifier::new(pinned(TPM_KIND, &ak))
            .with_boot_reference(refs)
            .unwrap();
        assert_eq!(verify(&text, &[&strict]).unwrap().achieved, Tier::T2);
        // Wrong boot reference: T1.
        let wrong = TpmVerifier::new(pinned(TPM_KIND, &ak))
            .with_boot_reference(BTreeMap::from([(0u32, hex(&[9u8; 32]))]))
            .unwrap();
        assert_eq!(verify(&text, &[&wrong]).unwrap().achieved, Tier::T1);
    }

    #[test]
    fn tpm_t2_needs_pcr23_to_hold_the_declared_measurements() {
        let tpm = SoftTpm::new(None); // measurements never extended
        let ak = point(&tpm.ak)[1..].to_vec();
        let refs = BTreeMap::from([(0u32, hex(&[1u8; 32]))]);
        let v = TpmVerifier::new(pinned(TPM_KIND, &ak))
            .with_boot_reference(refs)
            .unwrap();
        let r = verify(&statement(&tpm, "aa"), &[&v]).unwrap();
        assert_eq!(r.achieved, Tier::T1);
        assert!(r.notes.iter().any(|n| n.contains("does not hold")));
        // A host that extended different measurements than it declares is caught the same way.
        let mut lying = SoftTpm::new(Some(&Measurements {
            binary: "evil".into(),
            ..meas()
        }));
        lying.ak = tpm.ak.clone();
        assert_eq!(
            verify(&statement(&lying, "aa"), &[&v]).unwrap().achieved,
            Tier::T1
        );
    }

    #[test]
    fn tpm_quote_for_another_statement_is_rejected() {
        let mut tpm = SoftTpm::new(Some(&meas()));
        tpm.extra_override = Some(vec![0; 32]); // quoted something other than our binding
        let ak = point(&tpm.ak)[1..].to_vec();
        let v = TpmVerifier::new(pinned(TPM_KIND, &ak));
        let r = verify(&statement(&tpm, "aa"), &[&v]).unwrap();
        assert_eq!(r.achieved, Tier::T0);
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("not bound to this statement")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn tpm_evidence_tampering_is_rejected() {
        let tpm = SoftTpm::new(Some(&meas()));
        let ak = point(&tpm.ak)[1..].to_vec();
        let v = TpmVerifier::new(pinned(TPM_KIND, &ak));
        let st = Statement::parse(&statement(&tpm, "aa")).unwrap();
        let ev = String::from_utf8(unhex(&st.platform_evidence).unwrap()).unwrap();
        let resign = |ev: String| {
            let mut s = st.clone();
            s.platform_evidence = hex(ev.as_bytes());
            s.sign(&session()).unwrap()
        };
        // Altered PCR value: no longer hashes to the quoted digest.
        let mut bad = ev.clone();
        let i = bad.find("0:").unwrap();
        bad.replace_range(i + 2..i + 4, "ee");
        assert_eq!(verify(&resign(bad), &[&v]).unwrap().achieved, Tier::T0);
        // Flipped bit in the quote body: signature fails.
        let q = ev
            .lines()
            .find(|l| l.starts_with("quote "))
            .unwrap()
            .to_string();
        let mut flipped = q.clone();
        flipped.replace_range(30..31, if &q[30..31] == "0" { "1" } else { "0" });
        assert_eq!(
            verify(&resign(ev.replace(&q, &flipped)), &[&v])
                .unwrap()
                .achieved,
            Tier::T0
        );
        // Swapped attestation key: signature fails.
        let other = point(&device(9));
        let other_ak = hex(&tpm_ek::encode_ak(
            other[1..33].try_into().unwrap(),
            other[33..65].try_into().unwrap(),
            tpm_ek::AK_ATTRIBUTES,
        ));
        let swapped = ev
            .lines()
            .map(|l| {
                if l.starts_with("akpub ") {
                    format!("akpub {other_ak}")
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        assert_eq!(verify(&resign(swapped), &[&v]).unwrap().achieved, Tier::T0);
        // Truncated or garbage evidence is an error note, not a panic.
        assert_eq!(
            verify(&resign("akpub 00\n".into()), &[&v])
                .unwrap()
                .achieved,
            Tier::T0
        );
        assert_eq!(
            parse_quote(&[1, 2, 3]).unwrap_err(),
            "truncated TPM structure".into()
        );
    }

    #[test]
    fn a_quote_under_an_unrestricted_key_is_rejected() {
        // A key that is not restricted can sign anything, including bytes that look like a quote, so
        // such a "quote" proves nothing. It must be refused even though every signature is valid.
        for attrs in [
            tpm_ek::AK_ATTRIBUTES & !(1 << 16),
            tpm_ek::AK_ATTRIBUTES & !(1 << 1),
            tpm_ek::AK_ATTRIBUTES | (1 << 17),
        ] {
            let mut tpm = SoftTpm::new(Some(&meas()));
            tpm.ak_attributes = attrs;
            let ak = point(&tpm.ak)[1..].to_vec();
            let v = TpmVerifier::new(pinned(TPM_KIND, &ak));
            let r = verify(&statement(&tpm, "aa"), &[&v]).unwrap();
            assert_eq!(r.achieved, Tier::T0, "attrs {attrs:#x}");
            assert!(
                r.notes
                    .iter()
                    .any(|n| n.contains("rejected") && n.contains("restricted")),
                "{:?}",
                r.notes
            );
        }
    }

    #[test]
    fn a_missing_ek_certificate_blocks_the_genuineness_claim_when_anchors_are_set() {
        let tpm = SoftTpm::new(Some(&meas()));
        let ak = point(&tpm.ak)[1..].to_vec();
        // Anchors configured, but the evidence carries no EK certificate: pinned is not enough.
        let v = TpmVerifier::new(pinned(TPM_KIND, &ak))
            .with_ek_anchors(vec![vec![0x30, 0x03, 1, 2, 3]]);
        let r = verify(&statement(&tpm, "aa"), &[&v]).unwrap();
        assert_eq!(r.achieved, Tier::T0);
        assert!(r.challenge.is_none());
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("no genuine TPM is established")
                    && n.contains("no EK certificate")),
            "{:?}",
            r.notes
        );
        // Without anchors the old behaviour holds, and says what was not checked.
        let r = verify(
            &statement(&tpm, "bb"),
            &[&TpmVerifier::new(pinned(TPM_KIND, &ak))],
        )
        .unwrap();
        assert_eq!(r.achieved, Tier::T1);
        assert!(r
            .notes
            .iter()
            .any(|n| n.contains("no EK trust anchors configured")));
    }

    #[test]
    fn pins_round_trip_through_a_file() {
        let dir = std::env::temp_dir().join(format!("playbox-pins-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("pins");
        let mut p = PinStore::load(&path).unwrap();
        assert!(p.list().is_empty());
        p.add("tpm", "abc").unwrap();
        p.add("secure-enclave", "def").unwrap();
        let q = PinStore::load(&path).unwrap();
        assert!(
            q.contains("tpm", "abc")
                && q.contains("secure-enclave", "def")
                && !q.contains("tpm", "def")
        );
        let mut q = q;
        assert!(q.remove("tpm", "abc").unwrap());
        assert!(!PinStore::load(&path).unwrap().contains("tpm", "abc"));
        assert!(q.add("bad kind", "x").is_err());
    }

    #[test]
    fn boot_reference_is_validated() {
        let v = TpmVerifier::new(PinStore::in_memory());
        assert!(v
            .with_boot_reference(BTreeMap::from([(23u32, hex(&[0; 32]))]))
            .is_err());
        let v = TpmVerifier::new(PinStore::in_memory());
        assert!(v
            .with_boot_reference(BTreeMap::from([(0u32, "abcd".into())]))
            .is_err());
    }
}
