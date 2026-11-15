//! TPM 2.0 quote parsing and verification against boot references and EK anchors.

use super::pins::PinStore;
use super::{fingerprint, hex, need, parse_evidence, unhex};
use crate::attest::{
    AttestError, Challenge, Measurements, PlatformVerdict, PlatformVerifier, Statement, Tier,
};
use crate::tpm_ek;
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const TPM_KIND: &str = "tpm";
/// The PCR a host extends with its declared measurements. PCR 23 is the one meant for applications.
pub const MEASUREMENT_PCR: u32 = 23;
/// PCRs quoted: the firmware and boot chain (0-7) and the measurement PCR.
pub const QUOTED_PCRS: [u32; 9] = [0, 1, 2, 3, 4, 5, 6, 7, MEASUREMENT_PCR];

pub(super) const TPM_GENERATED: u32 = 0xFF54_4347;
pub(super) const TPM_ST_ATTEST_QUOTE: u16 = 0x8018;
pub(super) const TPM_ALG_SHA256: u16 = 0x000B;
pub(super) const TPM_ALG_ECDSA: u16 = 0x0018;

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

pub(super) struct Reader<'a>(&'a [u8]);

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
pub(super) struct Quote {
    extra_data: Vec<u8>,
    reset_count: u32,
    restart_count: u32,
    /// PCR indices of the single SHA-256 bank that was quoted.
    selected: BTreeSet<u32>,
    pub(super) pcr_digest: Vec<u8>,
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is bounded, or truncated on purpose"
)]
pub(super) fn parse_quote(bytes: &[u8]) -> Result<Quote, AttestError> {
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
pub(super) fn parse_ecdsa_signature(bytes: &[u8]) -> Result<[u8; 64], AttestError> {
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
