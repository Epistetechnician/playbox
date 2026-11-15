//! Apple Secure Enclave attestation: verifier and helper-backed provider.

use super::pins::PinStore;
use super::{fingerprint, hex, need, parse_evidence, unhex};
use crate::attest::{
    AttestError, Measurements, PlatformProvider, PlatformVerdict, PlatformVerifier, Produced,
    Statement, Tier,
};
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use std::{path::PathBuf, process::Command};

pub const SE_KIND: &str = "secure-enclave";
pub(super) const SE_DOMAIN: &[u8] = b"playbox.se.v1\n";

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
