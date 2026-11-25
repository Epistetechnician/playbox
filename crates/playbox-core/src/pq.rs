//! Stateless post-quantum signatures:
//! - FIPS 204 `ML-DSA-65` (Module-LWE lattice) for hybrid PQC dual-signing.
//! - FIPS 205 `SLH-DSA-SHA2-128f` (pure SHA-256 hash-based) for PQAI zero-new-assumption root/checkpoint custody.

use fips204::ml_dsa_65;
use fips204::traits::{KeyGen, SerDes, Signer, Verifier};
use fips205::slh_dsa_sha2_128f;
use fips205::traits::{
    KeyGen as SlhKeyGen, SerDes as SlhSerDes, Signer as SlhSigner, Verifier as SlhVerifier,
};
use thiserror::Error;

pub const MLDSA65_PK_LEN: usize = ml_dsa_65::PK_LEN;
pub const MLDSA65_SK_LEN: usize = ml_dsa_65::SK_LEN;
pub const MLDSA65_SIG_LEN: usize = ml_dsa_65::SIG_LEN;

pub const SLHDSA_PK_LEN: usize = slh_dsa_sha2_128f::PK_LEN;
pub const SLHDSA_SK_LEN: usize = slh_dsa_sha2_128f::SK_LEN;
pub const SLHDSA_SIG_LEN: usize = slh_dsa_sha2_128f::SIG_LEN;
pub const SLHDSA_ALG: &str = "SLH-DSA-SHA2-128f";

const HOST_SEED_DOMAIN: &str = "playbox.mldsa65.host.v1";
const SLH_SEED_DOMAIN_SK: &str = "playbox.slhdsa.sha2_128f.sk_seed.v1";
const SLH_SEED_DOMAIN_PRF: &str = "playbox.slhdsa.sha2_128f.sk_prf.v1";
const SLH_SEED_DOMAIN_PK: &str = "playbox.slhdsa.sha2_128f.pk_seed.v1";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PqError {
    #[error("ml-dsa-65 keygen failed: {0}")]
    Keygen(&'static str),
    #[error("ml-dsa-65 secret key must be {MLDSA65_SK_LEN} bytes")]
    BadSecretLen,
    #[error("invalid ml-dsa-65 secret key: {0}")]
    BadSecret(&'static str),
    #[error("ml-dsa-65 context exceeds 255 bytes")]
    ContextTooLong,
    #[error("ml-dsa-65 sign failed: {0}")]
    Sign(&'static str),
    #[error("slh-dsa-sha2-128f keygen failed: {0}")]
    SlhKeygen(&'static str),
    #[error("slh-dsa-sha2-128f secret key must be {SLHDSA_SK_LEN} bytes")]
    SlhBadSecretLen,
    #[error("invalid slh-dsa-sha2-128f secret key: {0}")]
    SlhBadSecret(&'static str),
    #[error("slh-dsa-sha2-128f context exceeds 255 bytes")]
    SlhContextTooLong,
    #[error("slh-dsa-sha2-128f sign failed: {0}")]
    SlhSign(&'static str),
}

/// Derive a deterministic ML-DSA-65 `(public_key, secret_key)` byte pair from a 32-byte Ed25519 signing seed.
#[must_use]
pub fn derive_mldsa65_from_ed25519_seed(ed25519_seed: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
    let xi = blake3::derive_key(HOST_SEED_DOMAIN, ed25519_seed);
    let (pk, sk) = ml_dsa_65::KG::keygen_from_seed(&xi);
    (pk.into_bytes().to_vec(), sk.into_bytes().to_vec())
}

/// Generate a fresh random ML-DSA-65 `(public_key, secret_key)` byte pair using OS randomness.
///
/// # Errors
///
/// Fails if the OS random number generator fails.
pub fn keygen_mldsa65() -> Result<(Vec<u8>, Vec<u8>), PqError> {
    let (pk, sk) = ml_dsa_65::try_keygen().map_err(PqError::Keygen)?;
    Ok((pk.into_bytes().to_vec(), sk.into_bytes().to_vec()))
}

/// Sign `msg` with a serialized ML-DSA-65 secret key under domain context `ctx`.
///
/// # Errors
///
/// Fails on invalid secret key length/encoding or context length > 255 bytes.
pub fn sign_mldsa65(sk_bytes: &[u8], msg: &[u8], ctx: &[u8]) -> Result<Vec<u8>, PqError> {
    let arr: [u8; MLDSA65_SK_LEN] = sk_bytes.try_into().map_err(|_| PqError::BadSecretLen)?;
    let sk = ml_dsa_65::PrivateKey::try_from_bytes(arr).map_err(PqError::BadSecret)?;
    if ctx.len() > 255 {
        return Err(PqError::ContextTooLong);
    }
    let sig = sk.try_sign(msg, ctx).map_err(PqError::Sign)?;
    Ok(sig.to_vec())
}

/// Extract the serialized ML-DSA-65 public key corresponding to `sk_bytes`.
///
/// # Errors
///
/// Fails on invalid secret key length or encoding.
pub fn pk_from_sk_mldsa65(sk_bytes: &[u8]) -> Result<Vec<u8>, PqError> {
    let arr: [u8; MLDSA65_SK_LEN] = sk_bytes.try_into().map_err(|_| PqError::BadSecretLen)?;
    let sk = ml_dsa_65::PrivateKey::try_from_bytes(arr).map_err(PqError::BadSecret)?;
    Ok(sk.get_public_key().into_bytes().to_vec())
}

/// Verify an ML-DSA-65 signature over `msg` and context `ctx`. Returns `false` on any malformed key, signature, or mismatch.
#[must_use]
pub fn verify_mldsa65(pk_bytes: &[u8], msg: &[u8], sig_bytes: &[u8], ctx: &[u8]) -> bool {
    if ctx.len() > 255 {
        return false;
    }
    let Ok(pk_arr) = <[u8; MLDSA65_PK_LEN]>::try_from(pk_bytes) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; MLDSA65_SIG_LEN]>::try_from(sig_bytes) else {
        return false;
    };
    let Ok(pk) = ml_dsa_65::PublicKey::try_from_bytes(pk_arr) else {
        return false;
    };
    pk.verify(msg, &sig_arr, ctx)
}

/// Deterministically derive a FIPS 205 `SLH-DSA-SHA2-128f` keypair from a 32-byte host/custodian Ed25519 seed
/// via three domain-separated BLAKE3 sub-seeds (`sk_seed`, `sk_prf`, `pk_seed`).
#[must_use]
pub fn derive_slhdsa_from_ed25519_seed(ed25519_seed: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
    let sk_full = blake3::derive_key(SLH_SEED_DOMAIN_SK, ed25519_seed);
    let prf_full = blake3::derive_key(SLH_SEED_DOMAIN_PRF, ed25519_seed);
    let pk_full = blake3::derive_key(SLH_SEED_DOMAIN_PK, ed25519_seed);
    let sk_seed: [u8; 16] = sk_full[..16].try_into().expect("slice len 16");
    let sk_prf: [u8; 16] = prf_full[..16].try_into().expect("slice len 16");
    let pk_seed: [u8; 16] = pk_full[..16].try_into().expect("slice len 16");
    let (pk, sk) = slh_dsa_sha2_128f::KG::keygen_from_seed(&sk_seed, &sk_prf, &pk_seed);
    (pk.into_bytes().to_vec(), sk.into_bytes().to_vec())
}

/// Generate a fresh random FIPS 205 `SLH-DSA-SHA2-128f` keypair (serialized `(pk, sk)` bytes).
///
/// # Errors
///
/// Fails if the OS random number generator fails.
pub fn keygen_slhdsa() -> Result<(Vec<u8>, Vec<u8>), PqError> {
    let (pk, sk) = slh_dsa_sha2_128f::try_keygen().map_err(PqError::SlhKeygen)?;
    Ok((pk.into_bytes().to_vec(), sk.into_bytes().to_vec()))
}

/// Sign `msg` with a serialized FIPS 205 `SLH-DSA-SHA2-128f` secret key under domain context `ctx`.
///
/// # Errors
///
/// Fails on invalid secret key length/encoding or context length > 255 bytes.
pub fn sign_slhdsa(sk_bytes: &[u8], msg: &[u8], ctx: &[u8]) -> Result<Vec<u8>, PqError> {
    let arr: [u8; SLHDSA_SK_LEN] = sk_bytes.try_into().map_err(|_| PqError::SlhBadSecretLen)?;
    let sk = slh_dsa_sha2_128f::PrivateKey::try_from_bytes(arr).map_err(PqError::SlhBadSecret)?;
    if ctx.len() > 255 {
        return Err(PqError::SlhContextTooLong);
    }
    let sig = sk.try_sign(msg, ctx, true).map_err(PqError::SlhSign)?;
    Ok(sig.to_vec())
}

/// Extract the serialized FIPS 205 `SLH-DSA-SHA2-128f` public key corresponding to `sk_bytes`.
///
/// # Errors
///
/// Fails on invalid secret key length or encoding.
pub fn pk_from_sk_slhdsa(sk_bytes: &[u8]) -> Result<Vec<u8>, PqError> {
    let arr: [u8; SLHDSA_SK_LEN] = sk_bytes.try_into().map_err(|_| PqError::SlhBadSecretLen)?;
    let sk = slh_dsa_sha2_128f::PrivateKey::try_from_bytes(arr).map_err(PqError::SlhBadSecret)?;
    Ok(sk.get_public_key().into_bytes().to_vec())
}

/// Verify a FIPS 205 `SLH-DSA-SHA2-128f` pure hash-based signature over `msg` and context `ctx`.
/// Returns `false` on any malformed key, signature, oversized context, or verification mismatch.
#[must_use]
pub fn verify_slhdsa(pk_bytes: &[u8], msg: &[u8], sig_bytes: &[u8], ctx: &[u8]) -> bool {
    if ctx.len() > 255 {
        return false;
    }
    let Ok(pk_arr) = <[u8; SLHDSA_PK_LEN]>::try_from(pk_bytes) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; SLHDSA_SIG_LEN]>::try_from(sig_bytes) else {
        return false;
    };
    let Ok(pk) = slh_dsa_sha2_128f::PublicKey::try_from_bytes(pk_arr) else {
        return false;
    };
    pk.verify(msg, &sig_arr, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mldsa65_sign_verify_and_seed_derivation_work_fail_closed() {
        let seed = [42u8; 32];
        let (pk1, sk1) = derive_mldsa65_from_ed25519_seed(&seed);
        let (pk2, sk2) = derive_mldsa65_from_ed25519_seed(&seed);
        assert_eq!(pk1, pk2);
        assert_eq!(sk1, sk2);
        assert_eq!(pk1.len(), MLDSA65_PK_LEN);
        assert_eq!(sk1.len(), MLDSA65_SK_LEN);

        let ctx = b"playbox.pq.test.v1";
        let sig = sign_mldsa65(&sk1, b"checkpoint", ctx).unwrap();
        assert_eq!(sig.len(), MLDSA65_SIG_LEN);
        assert!(verify_mldsa65(&pk1, b"checkpoint", &sig, ctx));
        assert!(!verify_mldsa65(&pk1, b"tampered", &sig, ctx));
        assert!(!verify_mldsa65(&pk1, b"checkpoint", &sig, b"wrong.ctx"));

        let mut bad_sig = sig.clone();
        bad_sig[0] ^= 1;
        assert!(!verify_mldsa65(&pk1, b"checkpoint", &bad_sig, ctx));

        let (rpk, rsk) = keygen_mldsa65().unwrap();
        let rsig = sign_mldsa65(&rsk, b"release", ctx).unwrap();
        assert!(verify_mldsa65(&rpk, b"release", &rsig, ctx));
        assert!(!verify_mldsa65(&pk1, b"release", &rsig, ctx));
    }

    #[test]
    fn slhdsa_sha2_128f_hash_root_works_fail_closed() {
        let seed = [7u8; 32];
        let (pk1, sk1) = derive_slhdsa_from_ed25519_seed(&seed);
        let (pk2, sk2) = derive_slhdsa_from_ed25519_seed(&seed);
        assert_eq!(pk1, pk2);
        assert_eq!(sk1, sk2);
        assert_eq!(pk1.len(), SLHDSA_PK_LEN);
        assert_eq!(sk1.len(), SLHDSA_SK_LEN);
        assert_eq!(pk_from_sk_slhdsa(&sk1).unwrap(), pk1);

        let ctx = b"playbox.pqai.test.v1";
        let sig = sign_slhdsa(&sk1, b"epoch-checkpoint", ctx).unwrap();
        assert_eq!(sig.len(), SLHDSA_SIG_LEN);
        assert!(verify_slhdsa(&pk1, b"epoch-checkpoint", &sig, ctx));
        assert!(!verify_slhdsa(&pk1, b"tampered", &sig, ctx));
        assert!(!verify_slhdsa(
            &pk1,
            b"epoch-checkpoint",
            &sig,
            b"wrong.ctx"
        ));
        assert!(!verify_slhdsa(&pk1, b"epoch-checkpoint", &sig, &[0u8; 256]));
        assert!(!verify_slhdsa(&pk1[..16], b"epoch-checkpoint", &sig, ctx));
        assert!(!verify_slhdsa(&pk1, b"epoch-checkpoint", &sig[..64], ctx));

        let mut bad_sig = sig.clone();
        bad_sig[0] ^= 1;
        assert!(!verify_slhdsa(&pk1, b"epoch-checkpoint", &bad_sig, ctx));

        let (rpk, rsk) = keygen_slhdsa().unwrap();
        let rsig = sign_slhdsa(&rsk, b"release", ctx).unwrap();
        assert!(verify_slhdsa(&rpk, b"release", &rsig, ctx));
        assert!(!verify_slhdsa(&pk1, b"release", &rsig, ctx));
    }
}
