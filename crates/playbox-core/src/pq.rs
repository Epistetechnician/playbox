//! Stateless post-quantum signatures (FIPS 204 ML-DSA-65) for hybrid dual-signing on RoomLog checkpoints and releases.

use fips204::ml_dsa_65;
use fips204::traits::{KeyGen, SerDes, Signer, Verifier};
use thiserror::Error;

pub const MLDSA65_PK_LEN: usize = ml_dsa_65::PK_LEN;
pub const MLDSA65_SK_LEN: usize = ml_dsa_65::SK_LEN;
pub const MLDSA65_SIG_LEN: usize = ml_dsa_65::SIG_LEN;

const HOST_SEED_DOMAIN: &str = "playbox.mldsa65.host.v1";

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
}
