//! TPM 2.0 endorsement keys: certificate chains and credential activation.
//!
//! The endorsement key (EK) is the TPM's manufacturer-certified identity key. An *attestation key*
//! (AK) is created by whoever uses the TPM, so a quote alone shows only that some AK signed it. To show
//! the AK lives in the same TPM as a certified EK, the verifier runs **credential activation**:
//!
//! 1. the verifier checks the EK certificate chains to a trust anchor it supplied;
//! 2. it encrypts a random secret to the EK's public key, bound to the AK's name (`make_credential`);
//! 3. only a TPM holding both that EK and that AK can decrypt it (`TPM2_ActivateCredential`);
//! 4. the verifier checks the secret that comes back.
//!
//! Layouts and key derivations follow TPM 2.0 Part 1 (credential protection, KDFa/KDFe), and
//! `make_credential` is checked against a real TPM's `ActivateCredential` in `tests/tpm_hardware.rs`.

use crate::attest::AttestError;
use aes::Aes128;
use cfb_mode::cipher::KeyIvInit;
use hmac::{Hmac, Mac};
use p256::{ecdh::EphemeralSecret, elliptic_curve::sec1::ToEncodedPoint, PublicKey};
use rand_core::{OsRng, RngCore};
use rsa::{pkcs1::DecodeRsaPublicKey, Oaep, RsaPublicKey};
use sha2::{Digest, Sha256};
use x509_parser::{prelude::*, time::ASN1Time};

type HmacSha256 = Hmac<Sha256>;

pub const TPM_ALG_ECC: u16 = 0x0023;
pub const TPM_ALG_NULL: u16 = 0x0010;
const TPM_ALG_SHA256: u16 = 0x000B;
const TPM_ALG_ECDSA: u16 = 0x0018;
const TPM_ECC_NIST_P256: u16 = 0x0003;

const FIXED_TPM: u32 = 1 << 1;
const FIXED_PARENT: u32 = 1 << 4;
const RESTRICTED: u32 = 1 << 16;
const DECRYPT: u32 = 1 << 17;
const SIGN: u32 = 1 << 18;

/// The magic and version that `tpm2_activatecredential` expects at the start of a credential file.
pub const CREDENTIAL_FILE_MAGIC: u32 = 0xBADC_C0DE;

#[must_use]
pub fn tpm2b(b: &[u8]) -> Vec<u8> {
    [
        &u16::try_from(b.len())
            .expect("length fits in u16")
            .to_be_bytes()[..],
        b,
    ]
    .concat()
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
    fn tpm2b(&mut self) -> Result<&'a [u8], AttestError> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

/// A TPM attestation key, parsed from its `TPMT_PUBLIC`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AkPublic {
    pub x: [u8; 32],
    pub y: [u8; 32],
    /// The TPM's name for the key: the name algorithm id followed by the hash of the `TPMT_PUBLIC`.
    pub name: Vec<u8>,
}

/// Encode the TPMT_PUBLIC of a P-256 ECDSA key with the given object attributes. The TPM produces these
/// itself; this exists for tests and for building fixtures.
#[doc(hidden)]
#[must_use]
pub fn encode_ak(x: &[u8; 32], y: &[u8; 32], attrs: u32) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend(TPM_ALG_ECC.to_be_bytes());
    t.extend(TPM_ALG_SHA256.to_be_bytes());
    t.extend(attrs.to_be_bytes());
    t.extend(tpm2b(&[]));
    t.extend(TPM_ALG_NULL.to_be_bytes());
    t.extend(TPM_ALG_ECDSA.to_be_bytes());
    t.extend(TPM_ALG_SHA256.to_be_bytes());
    t.extend(TPM_ECC_NIST_P256.to_be_bytes());
    t.extend(TPM_ALG_NULL.to_be_bytes());
    t.extend(tpm2b(x));
    t.extend(tpm2b(y));
    t
}

/// The attributes `tpm2_createak` gives a restricted signing key.
pub const AK_ATTRIBUTES: u32 = 0x0005_0072;

/// Parse a `TPMT_PUBLIC` and insist it describes a **restricted, TPM-resident, signing-only** P-256 key.
///
/// This matters: a key that is not restricted can sign arbitrary bytes, including bytes that look like
/// a TPM quote, so a quote under it proves nothing. Restricted keys sign only data the TPM generated.
/// `fixedTPM` and `fixedParent` mean the key cannot leave this TPM.
///
/// # Errors
///
/// Returns an [`AttestError`] if the input is malformed or the evidence does not verify.
pub fn parse_ak(tpmt: &[u8]) -> Result<AkPublic, AttestError> {
    let mut r = Reader(tpmt);
    if r.u16()? != TPM_ALG_ECC {
        return Err("attestation key is not an ECC key".into());
    }
    if r.u16()? != TPM_ALG_SHA256 {
        return Err("attestation key name algorithm is not SHA-256".into());
    }
    let attrs = r.u32()?;
    let _auth_policy = r.tpm2b()?;
    if r.u16()? != TPM_ALG_NULL {
        return Err(
            "attestation key has a symmetric algorithm, so it is not a pure signing key".into(),
        );
    }
    if r.u16()? != TPM_ALG_ECDSA || r.u16()? != TPM_ALG_SHA256 {
        return Err("attestation key scheme is not ECDSA with SHA-256".into());
    }
    if r.u16()? != TPM_ECC_NIST_P256 {
        return Err("attestation key curve is not NIST P-256".into());
    }
    if r.u16()? != TPM_ALG_NULL {
        return Err("attestation key has a KDF".into());
    }
    let x: [u8; 32] = r
        .tpm2b()?
        .try_into()
        .map_err(|_| "attestation key x is not 32 bytes")?;
    let y: [u8; 32] = r
        .tpm2b()?
        .try_into()
        .map_err(|_| "attestation key y is not 32 bytes")?;
    if !r.0.is_empty() {
        return Err("trailing bytes after TPMT_PUBLIC".into());
    }
    let need = FIXED_TPM | FIXED_PARENT | RESTRICTED | SIGN;
    if attrs & need != need || attrs & DECRYPT != 0 {
        return Err(format!(
            "attestation key is not a restricted, non-exportable signing key (attributes {attrs:#x}); a quote under it would prove nothing"
        ).into());
    }
    let mut name = TPM_ALG_SHA256.to_be_bytes().to_vec();
    name.extend(Sha256::digest(tpmt));
    Ok(AkPublic { x, y, name })
}

/// The public key of an endorsement key.
#[derive(Debug, Clone)]
pub enum EkKey {
    Rsa(RsaPublicKey),
    Ecc(PublicKey),
}

impl EkKey {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            EkKey::Rsa(_) => "rsa",
            EkKey::Ecc(_) => "ecc",
        }
    }
}

/// What a verified EK certificate tells us.
#[derive(Debug, Clone)]
pub struct EkCert {
    pub key: EkKey,
    pub subject: String,
    pub issuer: String,
    /// The TPM manufacturer / model / version from the certificate's subject alternative name, if present.
    pub tpm_info: Option<String>,
}

/// TPMs store the certificate in an NV index padded with zeros. Cut it at its DER length.
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
pub fn trim_der(b: &[u8]) -> Result<&[u8], AttestError> {
    if b.len() < 4 || b[0] != 0x30 {
        return Err("not a DER certificate".into());
    }
    let (hdr, len) = match b[1] {
        n if n < 0x80 => (2, n as usize),
        0x81 => (3, b[2] as usize),
        0x82 => (4, ((b[2] as usize) << 8) | b[3] as usize),
        _ => return Err("unsupported DER length".into()),
    };
    b.get(..hdr + len)
        .ok_or_else(|| "certificate is truncated".into())
}

/// Split PEM text into DER certificates.
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
pub fn pem_to_ders(pem: &str) -> Result<Vec<Vec<u8>>, AttestError> {
    let mut out = Vec::new();
    for item in Pem::iter_from_buffer(pem.as_bytes()) {
        let item = item.map_err(|e| format!("bad PEM: {e}"))?;
        if item.label == "CERTIFICATE" {
            out.push(item.contents);
        }
    }
    if out.is_empty() {
        return Err("no certificates found".into());
    }
    Ok(out)
}

fn parse_cert(der: &[u8]) -> Result<X509Certificate<'_>, AttestError> {
    let (_, c) =
        parse_x509_certificate(trim_der(der)?).map_err(|e| format!("bad certificate: {e}"))?;
    Ok(c)
}

/// Verify `leaf` chains to one of `anchors` through at most four of `chain`'s intermediates, that every
/// certificate is valid at `now_unix`, and that every intermediate is a CA. Returns the EK it certifies.
///
/// Not checked: revocation (there is no standard way to learn it for TPM EK certificates), and the TCG
/// EK-certificate profile beyond the key itself.
///
/// # Errors
///
/// Returns an [`AttestError`] if the input is malformed or the evidence does not verify.
pub fn check_ek_chain(
    leaf: &[u8],
    chain: &[Vec<u8>],
    anchors: &[Vec<u8>],
    now_unix: i64,
) -> Result<EkCert, AttestError> {
    let at = ASN1Time::from_timestamp(now_unix).map_err(|e| format!("bad time: {e}"))?;
    let leaf_c = parse_cert(leaf)?;
    if !leaf_c.validity().is_valid_at(at) {
        return Err("EK certificate is not valid at this time".into());
    }
    let anchor_cs: Vec<X509Certificate> =
        anchors.iter().filter_map(|d| parse_cert(d).ok()).collect();
    let chain_cs: Vec<X509Certificate> = chain.iter().filter_map(|d| parse_cert(d).ok()).collect();
    let signed_by = |cert: &X509Certificate, issuer: &X509Certificate| {
        cert.issuer() == issuer.subject()
            && issuer.validity().is_valid_at(at)
            && cert.verify_signature(Some(issuer.public_key())).is_ok()
    };
    let mut cur = &leaf_c;
    let mut reached = false;
    for _ in 0..5 {
        if anchor_cs.iter().any(|a| signed_by(cur, a)) {
            reached = true;
            break;
        }
        match chain_cs.iter().find(|c| c.is_ca() && signed_by(cur, c)) {
            Some(next) => cur = next,
            None => break,
        }
    }
    if !reached {
        return Err("EK certificate does not chain to a trust anchor you supplied".into());
    }

    let spki = leaf_c.public_key();
    let oid = spki.algorithm.algorithm.to_id_string();
    let key = match oid.as_str() {
        "1.2.840.113549.1.1.1" => {
            let k = RsaPublicKey::from_pkcs1_der(&spki.subject_public_key.data)
                .map_err(|e| format!("bad RSA EK: {e}"))?;
            if rsa::traits::PublicKeyParts::size(&k) != 256 {
                return Err("RSA EK is not 2048 bits".into());
            }
            EkKey::Rsa(k)
        }
        "1.2.840.10045.2.1" => EkKey::Ecc(
            PublicKey::from_sec1_bytes(&spki.subject_public_key.data)
                .map_err(|_| "EK is not a P-256 key")?,
        ),
        other => return Err(format!("unsupported EK key type {other}").into()),
    };
    let tpm_info = leaf_c
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|san| {
            san.value
                .general_names
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty());
    Ok(EkCert {
        key,
        subject: leaf_c.subject().to_string(),
        issuer: leaf_c.issuer().to_string(),
        tpm_info,
    })
}

/// TPM 2.0 Part 1 `KDFa` (SP 800-108 counter mode with HMAC-SHA-256).
#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is bounded, or truncated on purpose"
)]
fn kdfa(key: &[u8], label: &str, ctx_u: &[u8], ctx_v: &[u8], bits: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut counter = 1u32;
    while out.len() * 8 < bits {
        let mut m = HmacSha256::new_from_slice(key).expect("hmac takes any key length");
        m.update(&counter.to_be_bytes());
        m.update(label.as_bytes());
        m.update(&[0]);
        m.update(ctx_u);
        m.update(ctx_v);
        m.update(&(bits as u32).to_be_bytes());
        out.extend_from_slice(&m.finalize().into_bytes());
        counter += 1;
    }
    out.truncate(bits.div_ceil(8));
    out
}

/// TPM 2.0 Part 1 `KDFe` (SP 800-56A concatenation KDF with SHA-256).
fn kdfe(z: &[u8], label: &str, party_u: &[u8], party_v: &[u8], bits: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut counter = 1u32;
    while out.len() * 8 < bits {
        let mut h = Sha256::new();
        h.update(counter.to_be_bytes());
        h.update(z);
        h.update(label.as_bytes());
        h.update([0]);
        h.update(party_u);
        h.update(party_v);
        out.extend_from_slice(&h.finalize());
        counter += 1;
    }
    out.truncate(bits.div_ceil(8));
    out
}

/// A credential ready to be activated: `blob` is a `TPM2B_ID_OBJECT` and `secret` a `TPM2B_ENCRYPTED_SECRET`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub blob: Vec<u8>,
    pub secret: Vec<u8>,
}

impl Credential {
    /// The file layout `tpm2_activatecredential -i` reads.
    #[must_use]
    pub fn to_file(&self) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend(CREDENTIAL_FILE_MAGIC.to_be_bytes());
        f.extend(1u32.to_be_bytes());
        f.extend(&self.blob);
        f.extend(&self.secret);
        f
    }
}

/// `TPM2_MakeCredential` in software: protect `credential` (at most 64 bytes) so that only the TPM holding
/// `ek`, and the key named `ak_name` loaded in it, can recover it.
///
/// # Errors
///
/// Returns an [`AttestError`] if the evidence or statement cannot be produced or fails a check.
///
/// # Panics
///
/// Panics only if an internal invariant is broken, which indicates a bug.
pub fn make_credential(
    ek: &EkKey,
    ak_name: &[u8],
    credential: &[u8],
) -> Result<Credential, AttestError> {
    if credential.is_empty() || credential.len() > 64 {
        return Err("credential must be 1 to 64 bytes".into());
    }
    // The seed, and the part of it the TPM can unwrap.
    let (seed, secret) = match ek {
        EkKey::Rsa(k) => {
            let mut seed = [0u8; 32];
            OsRng.fill_bytes(&mut seed);
            let enc = k
                .encrypt(
                    &mut OsRng,
                    Oaep::new_with_label::<Sha256, _>("IDENTITY\0"),
                    &seed,
                )
                .map_err(|e| format!("RSA encryption failed: {e}"))?;
            (seed.to_vec(), tpm2b(&enc))
        }
        EkKey::Ecc(k) => {
            let eph = EphemeralSecret::random(&mut OsRng);
            let eph_pt = eph.public_key().to_encoded_point(false);
            let ek_pt = k.to_encoded_point(false);
            let (ex, ey) = (eph_pt.x().expect("x"), eph_pt.y().expect("y"));
            let z = eph.diffie_hellman(k);
            let seed = kdfe(
                z.raw_secret_bytes(),
                "IDENTITY",
                ex,
                ek_pt.x().expect("x"),
                256,
            );
            (seed, tpm2b(&[tpm2b(ex), tpm2b(ey)].concat()))
        }
    };
    let sym_key = kdfa(&seed, "STORAGE", ak_name, &[], 128);
    let mut enc = tpm2b(credential);
    cfb_mode::Encryptor::<Aes128>::new_from_slices(&sym_key, &[0u8; 16])
        .map_err(|e| e.to_string())?
        .encrypt(&mut enc);
    let hmac_key = kdfa(&seed, "INTEGRITY", &[], &[], 256);
    let mut mac = HmacSha256::new_from_slice(&hmac_key).expect("any length");
    mac.update(&enc);
    mac.update(ak_name);
    let integrity = mac.finalize().into_bytes();
    let blob = tpm2b(&[tpm2b(&integrity), enc].concat());
    Ok(Credential { blob, secret })
}

#[must_use]
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ak_tpmt(attrs: u32) -> Vec<u8> {
        encode_ak(&[1; 32], &[2; 32], attrs)
    }

    #[test]
    fn ak_must_be_restricted_fixed_and_signing_only() {
        let good = 0x0005_0072;
        let ak = parse_ak(&ak_tpmt(good)).unwrap();
        assert_eq!(ak.name[..2], [0x00, 0x0b]);
        assert_eq!(ak.name.len(), 34);
        // Not restricted: it could sign anything that looks like a quote.
        assert!(parse_ak(&ak_tpmt(good & !RESTRICTED))
            .unwrap_err()
            .to_string()
            .contains("restricted"));
        // Exportable.
        assert!(parse_ak(&ak_tpmt(good & !FIXED_TPM)).is_err());
        assert!(parse_ak(&ak_tpmt(good & !FIXED_PARENT)).is_err());
        // Not a signing key, or also a decryption key.
        assert!(parse_ak(&ak_tpmt(good & !SIGN)).is_err());
        assert!(parse_ak(&ak_tpmt(good | DECRYPT)).is_err());
        // Garbage.
        assert!(parse_ak(&ak_tpmt(good)[..20]).is_err());
        let mut trailing = ak_tpmt(good);
        trailing.push(0);
        assert!(parse_ak(&trailing).is_err());
    }

    #[test]
    fn der_padding_is_trimmed() {
        let cert = [0x30, 0x82, 0x00, 0x03, 1, 2, 3];
        assert_eq!(
            trim_der(&[&cert[..], &[0u8; 20][..]].concat()).unwrap(),
            &cert[..]
        );
        assert!(trim_der(&[0x31, 0, 0, 0]).is_err());
        assert!(trim_der(&[0x30, 0x82, 0x10, 0x00, 1]).is_err());
    }

    #[test]
    fn kdfs_have_the_requested_length_and_depend_on_every_input() {
        assert_eq!(kdfa(b"k", "STORAGE", b"n", b"", 128).len(), 16);
        assert_eq!(kdfa(b"k", "INTEGRITY", b"", b"", 256).len(), 32);
        assert_eq!(kdfa(b"k", "X", b"", b"", 520).len(), 65);
        let base = kdfa(b"k", "STORAGE", b"n", b"", 128);
        assert_ne!(base, kdfa(b"k2", "STORAGE", b"n", b"", 128));
        assert_ne!(base, kdfa(b"k", "STORAGE2", b"n", b"", 128));
        assert_ne!(base, kdfa(b"k", "STORAGE", b"n2", b"", 128));
        assert_eq!(kdfe(b"z", "IDENTITY", b"u", b"v", 256).len(), 32);
        assert_ne!(
            kdfe(b"z", "IDENTITY", b"u", b"v", 256),
            kdfe(b"z", "IDENTITY", b"u", b"w", 256)
        );
    }

    #[test]
    fn credentials_have_the_expected_shape() {
        let ek = EkKey::Ecc(
            PublicKey::from_sec1_bytes(
                p256::ecdsa::SigningKey::from_slice(&[7; 32])
                    .unwrap()
                    .verifying_key()
                    .to_encoded_point(false)
                    .as_bytes(),
            )
            .unwrap(),
        );
        let c = make_credential(&ek, &[0, 11, 1, 2, 3], &[9u8; 32]).unwrap();
        // blob: size | (size | 32-byte hmac) | encrypted(size | 32-byte credential)
        assert_eq!(
            u16::from_be_bytes([c.blob[0], c.blob[1]]) as usize,
            c.blob.len() - 2
        );
        assert_eq!(c.blob.len(), 2 + 2 + 32 + 2 + 32);
        // secret: size | (size | x) | (size | y)
        assert_eq!(c.secret.len(), 2 + 2 + 32 + 2 + 32);
        // Fresh randomness every time.
        assert_ne!(
            c,
            make_credential(&ek, &[0, 11, 1, 2, 3], &[9u8; 32]).unwrap()
        );
        assert!(make_credential(&ek, &[0], &[]).is_err());
        assert!(make_credential(&ek, &[0], &[0u8; 65]).is_err());
        assert_eq!(&c.to_file()[..8], &[0xba, 0xdc, 0xc0, 0xde, 0, 0, 0, 1]);
    }
}
