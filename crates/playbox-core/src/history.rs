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
        b.extend_from_slice(
            &u32::try_from(self.kind.len())
                .expect("length fits in u32")
                .to_be_bytes(),
        );
        b.extend_from_slice(self.kind.as_bytes());
        b.extend_from_slice(&self.payload_hash);
        b.extend_from_slice(&self.time_ms.to_be_bytes());
        b.extend_from_slice(&self.signer);
        b
    }

    /// Does the signature verify under the entry's own signer key? Does not check chaining.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn check_signature(&self) -> Result<(), VerifyError> {
        let key =
            VerifyingKey::from_bytes(&self.signer).map_err(|_| VerifyError::BadKey(self.seq))?;
        key.verify(&self.signing_bytes(), &Signature::from_bytes(&self.sig))
            .map_err(|_| VerifyError::BadSignature(self.seq))
    }

    /// Identity of the entry, covering everything including the signature.
    #[must_use]
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
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn head(&self) -> Hash {
        self.entries.last().map_or([0; 32], Entry::id)
    }

    /// # Panics
    ///
    /// Panics only if an internal invariant is broken, which indicates a bug.
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

    /// # Errors
    ///
    /// Returns an error if the operation fails.
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
            let key =
                VerifyingKey::from_bytes(&e.signer).map_err(|_| VerifyError::BadKey(e.seq))?;
            key.verify(&e.signing_bytes(), &Signature::from_bytes(&e.sig))
                .map_err(|_| VerifyError::BadSignature(e.seq))?;
            prev = e.id();
            last_time = e.time_ms;
        }
        Ok(())
    }
}

/// One entry per line: `seq prev kind payload_hash time_ms signer sig`, hashes and keys in hex.
/// `kind` is limited to `[a-z0-9_]+` so the line splits on spaces.
pub mod text {
    use super::{Entry, Hash, Log};
    use crate::record::RecordError;
    use std::fmt::Write;

    #[must_use]
    pub fn hex(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for x in b {
            let _ = write!(s, "{x:02x}");
        }
        s
    }

    fn unhex<const N: usize>(s: &str) -> Result<[u8; N], RecordError> {
        if s.len() != N * 2 || !s.is_ascii() {
            return Err(format!("expected {} hex chars", N * 2).into());
        }
        let mut out = [0u8; N];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
        }
        Ok(out)
    }

    #[must_use]
    pub fn valid_kind(k: &str) -> bool {
        !k.is_empty()
            && k.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }

    /// # Errors
    ///
    /// Returns a [`RecordError`] if the input is malformed or breaks a rule of the record's domain.
    pub fn entry_to_line(e: &Entry) -> Result<String, RecordError> {
        if !valid_kind(&e.kind) {
            return Err(format!("kind {:?} must match [a-z0-9_]+", e.kind).into());
        }
        Ok(format!(
            "{} {} {} {} {} {} {}",
            e.seq,
            hex(&e.prev),
            e.kind,
            hex(&e.payload_hash),
            e.time_ms,
            hex(&e.signer),
            hex(&e.sig)
        ))
    }

    /// # Errors
    ///
    /// Returns a [`RecordError`] if the input is malformed or breaks a rule of the record's domain.
    pub fn entry_from_line(line: &str) -> Result<Entry, RecordError> {
        let f: Vec<&str> = line.split(' ').collect();
        if f.len() != 7 {
            return Err(format!("expected 7 fields, found {}", f.len()).into());
        }
        if !valid_kind(f[2]) {
            return Err("bad kind".into());
        }
        Ok(Entry {
            seq: f[0].parse().map_err(|_| "bad seq".to_string())?,
            prev: unhex::<32>(f[1])?,
            kind: f[2].to_string(),
            payload_hash: unhex::<32>(f[3])?,
            time_ms: f[4].parse().map_err(|_| "bad time".to_string())?,
            signer: unhex::<32>(f[5])?,
            sig: unhex::<64>(f[6])?,
        })
    }

    /// # Errors
    ///
    /// Returns a [`RecordError`] if the input is malformed or breaks a rule of the record's domain.
    pub fn to_text(log: &Log) -> Result<String, RecordError> {
        let mut out = String::new();
        for e in &log.entries {
            out.push_str(&entry_to_line(e)?);
            out.push('\n');
        }
        Ok(out)
    }

    /// # Errors
    ///
    /// Returns a [`RecordError`] if the text or field is malformed, or its signature does not verify.
    pub fn from_text(s: &str) -> Result<Log, RecordError> {
        let mut log = Log::new();
        for (i, line) in s.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            log.entries
                .push(entry_from_line(line).map_err(|e| format!("line {}: {e}", i + 1))?);
        }
        Ok(log)
    }

    #[must_use]
    pub fn head_hex(log: &Log) -> String {
        let h: Hash = log.head();
        hex(&h)
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

    #[test]
    fn text_round_trip_preserves_verification() {
        let log = sample();
        let t = text::to_text(&log).unwrap();
        let back = text::from_text(&t).unwrap();
        assert_eq!(back.entries, log.entries);
        assert_eq!(back.verify(), Ok(()));
    }

    #[test]
    fn text_rejects_bad_lines_and_kinds() {
        assert!(text::from_text("1 2 3").is_err());
        let mut log = Log::new();
        log.append(&key(1), "has space", b"", 1);
        assert!(text::to_text(&log).is_err());
    }
}
