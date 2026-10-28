//! Proof of agentic history: a signed, hash-chained, append-only session log.
//!
//! Each entry commits to the previous entry's id, a payload hash, and the signer.
//! Payload bodies live elsewhere and may be redacted; the log only commits to hashes.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use thiserror::Error;

const DOMAIN: &[u8] = b"playbox.history.v1";

pub type Hash = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub prev: Hash,
    pub kind: String,
    pub payload_hash: Hash,
    pub time_ms: u64,
    pub signer: [u8; 32],
    pub sig: [u8; 64],
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("entry {0}: sequence number is not contiguous")]
    BadSeq(u64),
    #[error("entry {0}: prev hash does not match the previous entry")]
    BadPrev(u64),
    #[error("entry {0}: signer changed within a session")]
    SignerChanged(u64),
    #[error("entry {0}: invalid signer key")]
    BadKey(u64),
    #[error("entry {0}: signature does not verify")]
    BadSignature(u64),
    #[error("entry {0}: time goes backwards")]
    TimeBackwards(u64),
}

impl Entry {
    fn signing_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(DOMAIN.len() + 128 + self.kind.len());
        b.extend_from_slice(DOMAIN);
        b.extend_from_slice(&self.seq.to_be_bytes());
        b.extend_from_slice(&self.prev);
        b.extend_from_slice(&(self.kind.len() as u32).to_be_bytes());
        b.extend_from_slice(self.kind.as_bytes());
        b.extend_from_slice(&self.payload_hash);
        b.extend_from_slice(&self.time_ms.to_be_bytes());
        b.extend_from_slice(&self.signer);
        b
    }

    /// Identity of the entry, covering everything including the signature.
    pub fn id(&self) -> Hash {
        let mut h = blake3::Hasher::new();
        h.update(&self.signing_bytes());
        h.update(&self.sig);
        *h.finalize().as_bytes()
    }
}

/// An append-only log for one session, signed by one key.
#[derive(Debug, Default, Clone)]
pub struct Log {
    pub entries: Vec<Entry>,
}

impl Log {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn head(&self) -> Hash {
        self.entries.last().map(Entry::id).unwrap_or([0; 32])
    }

    pub fn append(&mut self, key: &SigningKey, kind: &str, payload: &[u8], time_ms: u64) -> &Entry {
        let mut e = Entry {
            seq: self.entries.len() as u64,
            prev: self.head(),
            kind: kind.to_string(),
            payload_hash: *blake3::hash(payload).as_bytes(),
            time_ms,
            signer: key.verifying_key().to_bytes(),
            sig: [0; 64],
        };
        e.sig = key.sign(&e.signing_bytes()).to_bytes();
        self.entries.push(e);
        self.entries.last().expect("just pushed")
    }

    pub fn verify(&self) -> Result<(), VerifyError> {
        let mut prev = [0u8; 32];
        let mut signer: Option<[u8; 32]> = None;
        let mut last_time = 0u64;
        for (i, e) in self.entries.iter().enumerate() {
            if e.seq != i as u64 {
                return Err(VerifyError::BadSeq(e.seq));
            }
            if e.prev != prev {
                return Err(VerifyError::BadPrev(e.seq));
            }
            match signer {
                None => signer = Some(e.signer),
                Some(s) if s != e.signer => return Err(VerifyError::SignerChanged(e.seq)),
                _ => {}
            }
            if e.time_ms < last_time {
                return Err(VerifyError::TimeBackwards(e.seq));
            }
            let key = VerifyingKey::from_bytes(&e.signer).map_err(|_| VerifyError::BadKey(e.seq))?;
            key.verify(&e.signing_bytes(), &Signature::from_bytes(&e.sig))
                .map_err(|_| VerifyError::BadSignature(e.seq))?;
            prev = e.id();
            last_time = e.time_ms;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn sample() -> Log {
        let k = key(1);
        let mut log = Log::new();
        log.append(&k, "session_start", b"claude-code", 1);
        log.append(&k, "tool_call", b"cargo test", 2);
        log.append(&k, "commit", b"tree:abc", 3);
        log
    }

    #[test]
    fn valid_log_verifies() {
        assert_eq!(sample().verify(), Ok(()));
    }

    #[test]
    fn tampered_payload_hash_is_rejected() {
        let mut log = sample();
        log.entries[1].payload_hash = [9; 32];
        assert_eq!(log.verify(), Err(VerifyError::BadSignature(1)));
    }

    #[test]
    fn removed_entry_is_rejected() {
        let mut log = sample();
        log.entries.remove(1);
        assert_eq!(log.verify(), Err(VerifyError::BadSeq(2)));
    }

    #[test]
    fn reordered_entries_are_rejected() {
        let mut log = sample();
        log.entries.swap(1, 2);
        assert!(log.verify().is_err());
    }

    #[test]
    fn mixed_signers_are_rejected() {
        let mut log = sample();
        let other = key(2);
        let prev = log.head();
        let mut e = Entry {
            seq: 3,
            prev,
            kind: "commit".into(),
            payload_hash: [0; 32],
            time_ms: 4,
            signer: other.verifying_key().to_bytes(),
            sig: [0; 64],
        };
        e.sig = other.sign(&e.signing_bytes()).to_bytes();
        log.entries.push(e);
        assert_eq!(log.verify(), Err(VerifyError::SignerChanged(3)));
    }

    #[test]
    fn time_going_backwards_is_rejected() {
        let k = key(1);
        let mut log = Log::new();
        log.append(&k, "a", b"", 10);
        log.append(&k, "b", b"", 5);
        assert_eq!(log.verify(), Err(VerifyError::TimeBackwards(1)));
    }
}
