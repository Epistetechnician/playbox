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
//! Not checked: the TPM's endorsement-key certificate chain (manufacturer roots) and Apple's App
//! Attest. Both would turn "pinned device key" into "genuine hardware"; neither is implemented.

use crate::attest::{
    AttestError, Challenge, Measurements, PlatformProvider, PlatformVerdict, PlatformVerifier,
    Produced, Statement, Tier,
};
use crate::tpm_ek;
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
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

/// The set of device keys an operator trusts, as `<kind> <fingerprint>` lines in a file.
#[derive(Debug, Clone, Default)]
pub struct PinStore {
    path: Option<PathBuf>,
    pins: BTreeSet<(String, String)>,
}

impl PinStore {
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load `path`; a missing file is an empty store. Saves go back to the same path.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if reading or writing fails or the data is invalid.
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut s = PinStore {
            path: Some(path.to_path_buf()),
            pins: BTreeSet::new(),
        };
        match fs::read_to_string(path) {
            Ok(t) => {
                for line in t
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                {
                    let (k, f) = line.split_once(' ').ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, format!("bad pin line {line:?}"))
                    })?;
                    s.pins.insert((k.to_string(), f.trim().to_string()));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(s)
    }

    #[must_use]
    pub fn contains(&self, kind: &str, fingerprint: &str) -> bool {
        self.pins
            .contains(&(kind.to_string(), fingerprint.to_string()))
    }

    #[must_use]
    pub fn list(&self) -> Vec<(String, String)> {
        self.pins.iter().cloned().collect()
    }

    /// # Errors
    ///
    /// Returns an I/O error if reading or writing fails or the data is invalid.
    pub fn add(&mut self, kind: &str, fingerprint: &str) -> io::Result<()> {
        if kind.is_empty()
            || fingerprint.is_empty()
            || kind.contains(char::is_whitespace)
            || fingerprint.contains(char::is_whitespace)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "kind and fingerprint must be non-empty words",
            ));
        }
        self.pins
            .insert((kind.to_string(), fingerprint.to_string()));
        self.save()
    }

    /// # Errors
    ///
    /// Returns an I/O error if reading or writing fails or the data is invalid.
    pub fn remove(&mut self, kind: &str, fingerprint: &str) -> io::Result<bool> {
        let had = self
            .pins
            .remove(&(kind.to_string(), fingerprint.to_string()));
        self.save()?;
        Ok(had)
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body: String = self
            .pins
            .iter()
            .map(|(k, f)| format!("{k} {f}\n"))
            .collect();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, body)?;
        fs::rename(&tmp, path)
    }
}

// ----------------------------------------------------------------------------------------------
// Secure Enclave (macOS)
// ----------------------------------------------------------------------------------------------

pub const SE_KIND: &str = "secure-enclave";
const SE_DOMAIN: &[u8] = b"playbox.se.v1\n";

/// The bytes the Secure Enclave key signs for a statement binding.
#[must_use]
pub fn se_message(binding: &[u8; 32]) -> Vec<u8> {
    [SE_DOMAIN, binding].concat()
}

/// Evidence: `pub <hex 65-byte x9.63 P-256 point>` and `sig <hex 64-byte r||s>`, ECDSA-SHA256 over
/// [`se_message`].
pub struct SecureEnclaveVerifier {
    pins: PinStore,
}

impl SecureEnclaveVerifier {
    #[must_use]
    pub fn new(pins: PinStore) -> Self {
        Self { pins }
    }
}

impl PlatformVerifier for SecureEnclaveVerifier {
    fn kind(&self) -> &str {
        SE_KIND
    }

    fn verify(&self, st: &Statement) -> Result<PlatformVerdict, AttestError> {
        let ev = parse_evidence(st)?;
        let public = unhex(need(&ev, "pub")?)?;
        let sig = unhex(need(&ev, "sig")?)?;
        let key = VerifyingKey::from_sec1_bytes(&public)
            .map_err(|_| "device public key is not a valid P-256 point")?;
        let sig = Signature::from_slice(&sig).map_err(|_| "device signature is malformed")?;
        key.verify(&se_message(&st.binding()), &sig)
            .map_err(|_| "device signature does not match this statement")?;
        let fp = fingerprint(&public);
        let mut notes = vec!["a P-256 device key signed this statement's binding".to_string()];
        let tier = if self.pins.contains(SE_KIND, &fp) {
            notes.push("device key is pinned: the same device key as the one you trusted".into());
            Tier::T1
        } else {
            notes.push(format!(
                "device key {fp} is not pinned, so it is not trusted; pin it to reach T1"
            ));
            Tier::T0
        };
        notes.push(
            "not checked: that the key lives in a Secure Enclave (needs Apple attestation)".into(),
        );
        Ok(PlatformVerdict {
            tier,
            notes,
            device: Some(fp),
            challenge: None,
        })
    }
}

/// Signs with a Secure Enclave key through the `playbox-se` helper (`tools/playbox-se`), which holds the
/// key as an opaque handle in `key_file`. macOS only.
pub struct SecureEnclaveProvider {
    pub helper: PathBuf,
    pub key_file: PathBuf,
}

impl PlatformProvider for SecureEnclaveProvider {
    fn kind(&self) -> &str {
        SE_KIND
    }

    fn produce(&self, binding: &[u8; 32], _m: &Measurements) -> Result<Produced, AttestError> {
        let out = Command::new(&self.helper)
            .args(["sign", "--key"])
            .arg(&self.key_file)
            .args(["--msg", &hex(&se_message(binding))])
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.helper.display()))?;
        if !out.status.success() {
            return Err(AttestError::Platform(format!(
                "playbox-se failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let text = String::from_utf8(out.stdout).map_err(|_| "playbox-se output is not utf-8")?;
        // Re-parse so a misbehaving helper cannot smuggle odd lines into the evidence.
        let mut public = None;
        let mut sig = None;
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("pub", v)) => public = Some(v.trim().to_string()),
                Some(("sig", v)) => sig = Some(v.trim().to_string()),
                _ => {}
            }
        }
        let (public, sig) = (
            public.ok_or("playbox-se gave no public key")?,
            sig.ok_or("playbox-se gave no signature")?,
        );
        if unhex(&public)?.len() != 65 || unhex(&sig)?.len() != 64 {
            return Err("playbox-se output has the wrong lengths".into());
        }
        Ok(Produced {
            evidence: format!("pub {public}\nsig {sig}\n"),
            claimed: Tier::T1,
        })
    }
}

// ----------------------------------------------------------------------------------------------
// TPM 2.0
// ----------------------------------------------------------------------------------------------

pub const TPM_KIND: &str = "tpm";
/// The PCR a host extends with its declared measurements. PCR 23 is the one meant for applications.
pub const MEASUREMENT_PCR: u32 = 23;
/// PCRs quoted: the firmware and boot chain (0-7) and the measurement PCR.
pub const QUOTED_PCRS: [u32; 9] = [0, 1, 2, 3, 4, 5, 6, 7, MEASUREMENT_PCR];

const TPM_GENERATED: u32 = 0xFF54_4347;
const TPM_ST_ATTEST_QUOTE: u16 = 0x8018;
const TPM_ALG_SHA256: u16 = 0x000B;
const TPM_ALG_ECDSA: u16 = 0x0018;

/// The digest a host extends into [`MEASUREMENT_PCR`] for its measurements.
#[must_use]
pub fn measurement_digest(m: &Measurements) -> [u8; 32] {
    let text = format!(
        "playbox.measurements.v1\nbinary {}\nrunner {}\npolicy {}\nos {}\narch {}\n",
        m.binary, m.runner, m.policy, m.os, m.arch
    );
    Sha256::digest(text.as_bytes()).into()
}

/// PCR value after one extend of `digest` into a PCR that was reset to zeros.
#[must_use]
pub fn extended_from_zero(digest: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([0u8; 32]);
    h.update(digest);
    h.finalize().into()
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], AttestError> {
        if self.0.len() < n {
            return Err("truncated TPM structure".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, AttestError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, AttestError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }
    fn u32(&mut self) -> Result<u32, AttestError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    fn u64(&mut self) -> Result<u64, AttestError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
    fn tpm2b(&mut self) -> Result<&'a [u8], AttestError> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

/// A parsed `TPMS_ATTEST` of type quote.
#[derive(Debug)]
struct Quote {
    extra_data: Vec<u8>,
    reset_count: u32,
    restart_count: u32,
    /// PCR indices of the single SHA-256 bank that was quoted.
    selected: BTreeSet<u32>,
    pcr_digest: Vec<u8>,
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is bounded, or truncated on purpose"
)]
fn parse_quote(bytes: &[u8]) -> Result<Quote, AttestError> {
    let mut r = Reader(bytes);
    if r.u32()? != TPM_GENERATED {
        return Err("not TPM-generated data (bad magic)".into());
    }
    if r.u16()? != TPM_ST_ATTEST_QUOTE {
        return Err("TPM structure is not a quote".into());
    }
    let _qualified_signer = r.tpm2b()?;
    let extra_data = r.tpm2b()?.to_vec();
    let _clock = r.u64()?;
    let reset_count = r.u32()?;
    let restart_count = r.u32()?;
    let _safe = r.u8()?;
    let _firmware = r.u64()?;
    let banks = r.u32()?;
    if banks != 1 {
        return Err(format!("expected exactly one PCR bank, found {banks}").into());
    }
    if r.u16()? != TPM_ALG_SHA256 {
        return Err("quoted PCR bank is not SHA-256".into());
    }
    let size = r.u8()? as usize;
    let mut selected = BTreeSet::new();
    for (byte_idx, b) in r.take(size)?.iter().enumerate() {
        for bit in 0..8 {
            if b & (1 << bit) != 0 {
                selected.insert((byte_idx * 8 + bit) as u32);
            }
        }
    }
    let pcr_digest = r.tpm2b()?.to_vec();
    if !r.0.is_empty() {
        return Err("trailing bytes after TPM quote".into());
    }
    Ok(Quote {
        extra_data,
        reset_count,
        restart_count,
        selected,
        pcr_digest,
    })
}

/// `TPMT_SIGNATURE` for ECDSA-SHA256 into a 64-byte r||s.
fn parse_ecdsa_signature(bytes: &[u8]) -> Result<[u8; 64], AttestError> {
    let mut r = Reader(bytes);
    if r.u16()? != TPM_ALG_ECDSA {
        return Err("quote is not signed with ECDSA".into());
    }
    if r.u16()? != TPM_ALG_SHA256 {
        return Err("quote signature does not use SHA-256".into());
    }
    let mut out = [0u8; 64];
    for half in 0..2 {
        let v = r.tpm2b()?;
        if v.len() > 32 {
            return Err("signature component is too long".into());
        }
        out[half * 32 + (32 - v.len())..(half + 1) * 32].copy_from_slice(v);
    }
    if !r.0.is_empty() {
        return Err("trailing bytes after TPM signature".into());
    }
    Ok(out)
}

/// Evidence lines: `akpub` (hex `TPMT_PUBLIC` of the attestation key), `quote` (hex `TPMS_ATTEST`), `sig`
/// (hex `TPMT_SIGNATURE`), `pcrs` (`index:hex32` entries, comma separated, ascending), and optionally
/// `ek_cert` (hex DER endorsement-key certificate) and `ek_chain` (hex DER intermediates, comma
/// separated). The quote's qualifying data must equal the statement binding.
///
/// The attestation key must be a restricted, non-exportable signing key ([`tpm_ek::parse_ak`]); without
/// that a quote under it proves nothing.
///
/// With trust anchors configured ([`TpmVerifier::with_ek_anchors`]) the verifier also checks the EK
/// certificate and issues a credential-activation [`Challenge`]: only after the host answers it, proving
/// the attestation key and the certified endorsement key share a TPM, does the tier rise above T0.
pub struct TpmVerifier {
    pins: PinStore,
    /// Reference values for boot PCRs (0-7) supplied by the operator, as hex SHA-256 values.
    boot_reference: BTreeMap<u32, String>,
    /// DER certificates (roots or intermediates) the operator trusts to vouch for TPM manufacturers.
    ek_anchors: Vec<Vec<u8>>,
}

impl TpmVerifier {
    #[must_use]
    pub fn new(pins: PinStore) -> Self {
        Self {
            pins,
            boot_reference: BTreeMap::new(),
            ek_anchors: Vec::new(),
        }
    }

    /// Trust these certificates (DER) to vouch for EK certificates. With any set, a TPM host must prove
    /// genuineness (EK chain plus credential activation) to rise above T0.
    #[must_use]
    pub fn with_ek_anchors(mut self, anchors: Vec<Vec<u8>>) -> Self {
        self.ek_anchors = anchors;
        self
    }

    /// Reference values the host's boot PCRs must match for T2. Indices must be 0-7.
    ///
    /// # Errors
    ///
    /// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
    pub fn with_boot_reference(mut self, refs: BTreeMap<u32, String>) -> Result<Self, AttestError> {
        for (i, v) in &refs {
            if *i > 7 {
                return Err(format!("PCR {i} is not a boot PCR (0-7)").into());
            }
            if unhex(v)?.len() != 32 {
                return Err(format!("reference for PCR {i} must be 32 bytes").into());
            }
        }
        self.boot_reference = refs
            .into_iter()
            .map(|(i, v)| (i, v.to_ascii_lowercase()))
            .collect();
        Ok(self)
    }

    /// Check the EK certificate in the evidence and build a credential-activation challenge for `ak`.
    fn ek_challenge(
        &self,
        ev: &BTreeMap<String, String>,
        ak: &tpm_ek::AkPublic,
    ) -> Result<(tpm_ek::EkCert, Vec<u8>, Vec<u8>), AttestError> {
        let leaf = unhex(ev.get("ek_cert").ok_or("the host sent no EK certificate")?)?;
        let mut chain = Vec::new();
        for c in ev
            .get("ek_chain")
            .map_or("", String::as_str)
            .split(',')
            .filter(|c| !c.is_empty())
        {
            if chain.len() >= 4 {
                return Err("too many intermediate certificates".into());
            }
            chain.push(unhex(c)?);
        }
        if leaf.len() > 8192 || chain.iter().any(|c| c.len() > 8192) {
            return Err("certificate is unreasonably large".into());
        }
        let cert = tpm_ek::check_ek_chain(&leaf, &chain, &self.ek_anchors, tpm_ek::now_unix())?;
        let mut secret = [0u8; 32];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut secret);
        let cred = tpm_ek::make_credential(&cert.key, &ak.name, &secret)?;
        let payload = format!(
            "ek {}\ncred {}\nsecret {}\n",
            cert.key.kind(),
            hex(&cred.blob),
            hex(&cred.secret)
        )
        .into_bytes();
        Ok((cert, payload, secret.to_vec()))
    }
}

impl PlatformVerifier for TpmVerifier {
    fn kind(&self) -> &str {
        TPM_KIND
    }

    fn verify(&self, st: &Statement) -> Result<PlatformVerdict, AttestError> {
        let ev = parse_evidence(st)?;
        let ak = tpm_ek::parse_ak(&unhex(need(&ev, "akpub")?)?)?;
        let ak_xy = [ak.x, ak.y].concat();
        let ak_point = [&[0x04u8][..], &ak_xy].concat();
        let key = VerifyingKey::from_sec1_bytes(&ak_point)
            .map_err(|_| "attestation key is not a valid P-256 point")?;
        let quote_bytes = unhex(need(&ev, "quote")?)?;
        let sig = Signature::from_slice(&parse_ecdsa_signature(&unhex(need(&ev, "sig")?)?)?)
            .map_err(|_| "malformed signature")?;
        key.verify(&quote_bytes, &sig)
            .map_err(|_| "quote signature does not verify under the attestation key")?;

        let quote = parse_quote(&quote_bytes)?;
        if quote.extra_data != st.binding() {
            return Err("quote is not bound to this statement (qualifying data differs)".into());
        }

        let mut pcrs: BTreeMap<u32, [u8; 32]> = BTreeMap::new();
        for entry in need(&ev, "pcrs")?.split(',').filter(|e| !e.is_empty()) {
            let (i, v) = entry.split_once(':').ok_or("bad pcr entry")?;
            let v: [u8; 32] = unhex(v)?
                .try_into()
                .map_err(|_| "PCR value must be 32 bytes")?;
            if pcrs
                .insert(i.parse().map_err(|_| "bad PCR index")?, v)
                .is_some()
            {
                return Err("duplicate PCR in evidence".into());
            }
        }
        if pcrs.keys().copied().collect::<BTreeSet<_>>() != quote.selected {
            return Err("PCR values do not match the PCRs the TPM quoted".into());
        }
        let mut h = Sha256::new();
        for v in pcrs.values() {
            h.update(v);
        }
        let computed: [u8; 32] = h.finalize().into();
        if computed.as_slice() != quote.pcr_digest.as_slice() {
            return Err("PCR values do not hash to the quoted digest".into());
        }

        let fp = fingerprint(&ak_xy);
        let mut notes = vec![format!(
            "TPM quote verified over PCRs {:?} (reset count {}, restart count {}); attestation key is restricted and non-exportable",
            pcrs.keys().collect::<Vec<_>>(),
            quote.reset_count,
            quote.restart_count
        )];
        let mut tier = Tier::T0;
        if self.pins.contains(TPM_KIND, &fp) {
            tier = Tier::T1;
            notes.push("attestation key is pinned: the same TPM as the one you trusted".into());
            let want = extended_from_zero(&measurement_digest(&st.measurements));
            let measured = pcrs.get(&MEASUREMENT_PCR) == Some(&want);
            let boot_ok = !self.boot_reference.is_empty()
                && self
                    .boot_reference
                    .iter()
                    .all(|(i, v)| pcrs.get(i).map(|p| hex(p)).as_deref() == Some(v.as_str()));
            match (measured, boot_ok) {
                (true, true) => {
                    tier = Tier::T2;
                    notes.push("PCR 23 holds this host's declared measurements and the boot PCRs match your reference".into());
                }
                (true, false) if self.boot_reference.is_empty() => {
                    notes.push("PCR 23 matches the declared measurements, but no boot reference values are configured, so T2 is not granted".into());
                }
                (true, false) => notes.push(
                    "PCR 23 matches, but the boot PCRs differ from your reference values".into(),
                ),
                (false, _) => {
                    notes.push("PCR 23 does not hold this host's declared measurements".into())
                }
            }
        } else {
            notes.push(format!(
                "attestation key {fp} is not pinned, so it is not trusted; pin it to reach T1"
            ));
        }

        let mut challenge = None;
        if self.ek_anchors.is_empty() {
            notes.push("not checked: that this is a genuine TPM (no EK trust anchors configured; see PLAYBOX_TPM_EK_ANCHORS)".into());
        } else if tier == Tier::T0 {
            notes.push(
                "EK genuineness check skipped: nothing to raise, the key is not pinned".into(),
            );
        } else {
            match self.ek_challenge(&ev, &ak) {
                Ok((cert, payload, expect)) => {
                    notes.push(format!(
                        "EK certificate chains to your trust anchor (subject {}, issuer {}{})",
                        cert.subject,
                        cert.issuer,
                        cert.tpm_info.map(|i| format!(", {i}")).unwrap_or_default()
                    ));
                    notes.push("genuineness is proven only when the TPM answers the credential-activation challenge".into());
                    challenge = Some(Challenge {
                        payload,
                        expect,
                        tier_on_success: tier,
                    });
                    tier = Tier::T0;
                }
                Err(e) => {
                    notes.push(format!(
                        "EK check failed, so no genuine TPM is established: {e}"
                    ));
                    tier = Tier::T0;
                }
            }
        }
        Ok(PlatformVerdict {
            tier,
            notes,
            device: Some(fp),
            challenge,
        })
    }
}

/// Produces TPM quotes with the `tpm2-tools` programs (Linux). Honors `TPM2TOOLS_TCTI`, so it also
/// works against a software TPM such as `swtpm`. Creates a persistent attestation key on first use.
pub struct TpmProvider {
    /// Persistent handle for the attestation key.
    pub ak_handle: String,
    lock: Mutex<()>,
}

impl Default for TpmProvider {
    fn default() -> Self {
        Self {
            ak_handle: "0x81010002".into(),
            lock: Mutex::new(()),
        }
    }
}

fn run(prog: &str, args: &[&str]) -> Result<Vec<u8>, AttestError> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {prog}: {e} (is tpm2-tools installed?)"))?;
    if !out.status.success() {
        return Err(format!(
            "{prog} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(out.stdout)
}

/// Clear transient objects a tool left loaded (see [`TpmProvider::produce`]). Failure is fine.
fn flush() {
    let _ = run("tpm2_flushcontext", &["-t"]);
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> io::Result<Self> {
        let mut b = [0u8; 8];
        fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
        let p = std::env::temp_dir().join(format!("playbox-tpm-{}", hex(&b)));
        fs::create_dir(&p)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(p))
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl TpmProvider {
    fn ensure_ak(&self, tmp: &TempDir) -> Result<(), AttestError> {
        if run(
            "tpm2_readpublic",
            &["-c", &self.ak_handle, "-o", &tmp.path("probe.pub")],
        )
        .is_ok()
        {
            return Ok(());
        }
        // The attestation key is created under the endorsement key (the TPM's identity key).
        run("tpm2_createek", &["-c", &tmp.path("ek.ctx"), "-G", "ecc"])?;
        let _ = run("tpm2_flushcontext", &["-t"]);
        run(
            "tpm2_createak",
            &[
                "-C",
                &tmp.path("ek.ctx"),
                "-c",
                &tmp.path("ak.ctx"),
                "-G",
                "ecc",
                "-g",
                "sha256",
                "-s",
                "ecdsa",
            ],
        )?;
        run(
            "tpm2_evictcontrol",
            &["-C", "o", "-c", &tmp.path("ak.ctx"), &self.ak_handle],
        )?;
        Ok(())
    }
}

/// NV indices where TCG puts the EK certificates: RSA 2048 first, then ECC P-256.
const EK_CERT_NV_INDICES: [&str; 2] = ["0x01c00002", "0x01c0000a"];

/// Intermediate certificates the operator supplies for the EK certificate: `$PLAYBOX_TPM_EK_CHAIN`, a PEM file.
fn ek_chain_from_env() -> Option<Vec<Vec<u8>>> {
    let path = std::env::var_os("PLAYBOX_TPM_EK_CHAIN")?;
    let ders = tpm_ek::pem_to_ders(&fs::read_to_string(path).ok()?).ok()?;
    Some(ders.into_iter().take(4).collect())
}

impl TpmProvider {
    fn read_ek_cert(&self, tmp: &TempDir) -> Option<Vec<u8>> {
        for (i, idx) in EK_CERT_NV_INDICES.iter().enumerate() {
            let out = tmp.path(&format!("ek{i}.der"));
            if run("tpm2_nvread", &[idx, "-C", "o", "-o", &out]).is_ok() {
                if let Ok(der) = fs::read(&out) {
                    if let Ok(t) = tpm_ek::trim_der(&der) {
                        return Some(t.to_vec());
                    }
                }
            }
        }
        None
    }
}

impl PlatformProvider for TpmProvider {
    fn kind(&self) -> &str {
        TPM_KIND
    }

    /// Credential activation: decrypt a credential a verifier made for our endorsement key and attestation
    /// key. Only this TPM can, which is the proof.
    fn respond(&self, request: &[u8]) -> Result<Vec<u8>, AttestError> {
        if request.len() > 8192 {
            return Err("challenge is too large".into());
        }
        let text = std::str::from_utf8(request).map_err(|_| "challenge is not utf-8")?;
        let mut f: BTreeMap<&str, &str> = BTreeMap::new();
        for line in text.lines() {
            let (k, v) = line.split_once(' ').ok_or("bad challenge line")?;
            if f.insert(k, v).is_some() {
                return Err("duplicate challenge field".into());
            }
        }
        let ek_kind = *f.get("ek").ok_or("challenge has no `ek`")?;
        if ek_kind != "rsa" && ek_kind != "ecc" {
            return Err("challenge `ek` must be rsa or ecc".into());
        }
        let cred = tpm_ek::Credential {
            blob: unhex(f.get("cred").ok_or("challenge has no `cred`")?)?,
            secret: unhex(f.get("secret").ok_or("challenge has no `secret`")?)?,
        };
        let _guard = self.lock.lock().map_err(|_| "tpm lock poisoned")?;
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let (credf, ekctx, sess, out) = (
            tmp.path("cred.bin"),
            tmp.path("ek.ctx"),
            tmp.path("sess.ctx"),
            tmp.path("secret.out"),
        );
        fs::write(&credf, cred.to_file()).map_err(|e| e.to_string())?;
        flush();
        run("tpm2_createek", &["-c", &ekctx, "-G", ek_kind])?;
        flush();
        // The endorsement key can only be used under a policy session that proves knowledge of the
        // endorsement hierarchy's authorization (empty on a normal TPM).
        run("tpm2_startauthsession", &["--policy-session", "-S", &sess])?;
        let result = run("tpm2_policysecret", &["-S", &sess, "-c", "e"]).and_then(|_| {
            run(
                "tpm2_activatecredential",
                &[
                    "-c",
                    &self.ak_handle,
                    "-C",
                    &ekctx,
                    "-i",
                    &credf,
                    "-o",
                    &out,
                    "-P",
                    &format!("session:{sess}"),
                ],
            )
        });
        let _ = run("tpm2_flushcontext", &["-l"]);
        flush();
        result?;
        let secret = fs::read(&out).map_err(|e| e.to_string())?;
        if secret.is_empty() || secret.len() > 64 {
            return Err("TPM returned an unexpected credential".into());
        }
        Ok(secret)
    }

    fn produce(&self, binding: &[u8; 32], m: &Measurements) -> Result<Produced, AttestError> {
        let _guard = self.lock.lock().map_err(|_| "tpm lock poisoned")?;
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        // Without a resource manager (a software TPM, for one) transient objects from an earlier,
        // interrupted call stay loaded and eventually fill the TPM. Calls are serialised by the lock
        // above, so clearing them is safe. Failure is fine: the kernel resource manager refuses it.
        flush();
        self.ensure_ak(&tmp)?;
        flush();

        // Put the declared measurements into the application PCR. If that is not possible the quote is
        // still useful as T1 evidence, but it must not claim T2.
        let digest = measurement_digest(m);
        let measured = run("tpm2_pcrreset", &[&MEASUREMENT_PCR.to_string()]).is_ok()
            && run(
                "tpm2_pcrextend",
                &[&format!("{MEASUREMENT_PCR}:sha256={}", hex(&digest))],
            )
            .is_ok();

        flush();
        let list = format!(
            "sha256:{}",
            QUOTED_PCRS
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let (msg, sigf, pcrf) = (
            tmp.path("quote.msg"),
            tmp.path("quote.sig"),
            tmp.path("pcrs.bin"),
        );
        // The quote and the PCR values are read by separate commands, so a PCR could change between
        // them. Check the values hash to the quoted digest, and try once more if not.
        let mut raw = Vec::new();
        let mut quote_bytes = Vec::new();
        for attempt in 0..2 {
            flush();
            run(
                "tpm2_quote",
                &[
                    "-c",
                    &self.ak_handle,
                    "-l",
                    &list,
                    "-q",
                    &hex(binding),
                    "-m",
                    &msg,
                    "-s",
                    &sigf,
                    "-g",
                    "sha256",
                ],
            )?;
            flush();
            run("tpm2_pcrread", &[&list, "-o", &pcrf])?;
            raw = fs::read(&pcrf).map_err(|e| e.to_string())?;
            quote_bytes = fs::read(&msg).map_err(|e| e.to_string())?;
            if raw.len() != 32 * QUOTED_PCRS.len() {
                return Err(AttestError::Platform(format!(
                    "unexpected PCR output size {}",
                    raw.len()
                )));
            }
            let quoted = parse_quote(&quote_bytes)?;
            let digest: [u8; 32] = Sha256::digest(&raw).into();
            if digest.as_slice() == quoted.pcr_digest.as_slice() {
                break;
            }
            if attempt == 1 {
                return Err("PCR values keep changing between the quote and the read".into());
            }
        }
        flush();
        let tpmt_path = tmp.path("ak.tpmt");
        run(
            "tpm2_readpublic",
            &["-c", &self.ak_handle, "-f", "tpmt", "-o", &tpmt_path],
        )?;
        let akpub = fs::read(&tpmt_path).map_err(|e| e.to_string())?;
        // Refuse to produce evidence a verifier would reject anyway, with a clearer reason.
        tpm_ek::parse_ak(&akpub)
            .map_err(|e| format!("this TPM's attestation key is unsuitable: {e}"))?;
        let pcrs: Vec<String> = QUOTED_PCRS
            .iter()
            .zip(raw.chunks(32))
            .map(|(i, v)| format!("{i}:{}", hex(v)))
            .collect();
        let mut evidence = format!(
            "akpub {}\nquote {}\nsig {}\npcrs {}\n",
            hex(&akpub),
            hex(&quote_bytes),
            hex(&fs::read(&sigf).map_err(|e| e.to_string())?),
            pcrs.join(",")
        );
        // The endorsement-key certificate, if the TPM has one, so a verifier with trust anchors can
        // check genuineness. Intermediates are not stored in TPMs; an operator can supply them.
        flush();
        if let Some(cert) = self.read_ek_cert(&tmp) {
            evidence.push_str(&format!("ek_cert {}\n", hex(&cert)));
            if let Some(chain) = ek_chain_from_env() {
                evidence.push_str(&format!(
                    "ek_chain {}\n",
                    chain.iter().map(|c| hex(c)).collect::<Vec<_>>().join(",")
                ));
            }
        }
        Ok(Produced {
            evidence,
            claimed: if measured { Tier::T2 } else { Tier::T1 },
        })
    }
}

// ----------------------------------------------------------------------------------------------
// Choosing a provider
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
        "auto" => Ok(if cfg!(target_os = "macos") {
            se().ok()
        } else if std::env::var_os("TPM2TOOLS_TCTI").is_some() || Path::new("/dev/tpmrm0").exists()
        {
            tpm().ok()
        } else {
            None
        }),
        other => {
            Err(format!("PLAYBOX_ATTEST must be none, auto, se, tpm or tdx, not {other:?}").into())
        }
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
    if let Some(cs) = confidential_space_from_env()? {
        v.push(Box::new(cs));
    }
    Ok(v)
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
    use super::*;
    use crate::attest::{self, new_statement, verify};
    use ed25519_dalek::SigningKey;
    use p256::ecdsa::{signature::Signer, SigningKey as P256Signing};

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
        k.verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
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
