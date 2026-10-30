//! Witnessed checkpoints: a host cannot show different members different histories.
//!
//! Every member mirrors the host's signed log, but nothing stops a malicious host from signing two
//! different logs and showing each to a different member (a *split view*). Transparency logs solve this
//! with witnesses: independent parties who cosign a log's checkpoint only if it extends the one they saw
//! before. A member who requires cosignatures from `m` of `n` witnesses therefore knows the checkpoint is
//! the same one the others see.
//!
//! - A [`Checkpoint`] is the log key's signature over `(size, head)`.
//! - A [`Witness`] cosigns a checkpoint only after checking the entries that extend its last view
//!   (contiguous sequence numbers, chain links, the log key's signature on each). It refuses to move
//!   backwards and, if shown two different heads at the same size, returns an [`Equivocation`]: two
//!   checkpoints signed by the log key that anyone can verify, non-repudiable proof the host forked.
//! - [`verify_quorum`] is the member's check: a valid checkpoint plus `m` distinct pinned witnesses.
//!
//! # Limits
//! A witness checks consistency by replaying the new entries, which is linear in the growth since its last
//! view (a Merkle log would make that logarithmic). Witnesses must be independent of the host and of each
//! other to mean anything; with one witness operated by the host, this proves nothing. A witness only
//! sees what it is shown, so a host can still stall one by withholding entries.

use crate::record::RecordError;
use crate::{
    history::{Entry, Hash, Log},
    record::{self, hash_parts, hex},
};
use ed25519_dalek::SigningKey;
use std::collections::{BTreeMap, BTreeSet};

const CP: &str = "playbox.checkpoint.v1";
const COSIG: &str = "playbox.cosignature.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// The log's signing key.
    pub origin: [u8; 32],
    pub size: u64,
    pub head: Hash,
    pub time_ms: u64,
    pub text: String,
}

impl Checkpoint {
    /// # Errors
    ///
    /// Returns a [`RecordError`] if the input is malformed or breaks a rule of the record's domain.
    pub fn sign(log: &Log, key: &SigningKey, time_ms: u64) -> Result<Checkpoint, RecordError> {
        let text = record::sign(
            CP,
            &[
                ("size", log.entries.len().to_string()),
                ("head", hex(&log.head())),
                ("time", time_ms.to_string()),
            ],
            key,
        )?;
        Self::open(&text)
    }

    /// # Errors
    ///
    /// Returns a [`RecordError`] if the text or field is malformed, or its signature does not verify.
    pub fn open(text: &str) -> Result<Checkpoint, RecordError> {
        let r = record::open(CP, text)?;
        Ok(Checkpoint {
            origin: r.signer,
            size: r.num("size")?,
            head: r.hash("head")?,
            time_ms: r.num("time")?,
            text: text.to_string(),
        })
    }

    fn digest(&self) -> Hash {
        hash_parts("playbox.checkpoint.digest", &[self.text.as_bytes()])
    }
}

/// Proof that a log's key signed two different heads for the same size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Equivocation {
    pub a: String,
    pub b: String,
}

impl Equivocation {
    /// Check two checkpoint texts: both validly signed by one key, same size, different heads.
    #[must_use]
    pub fn detect(a: &str, b: &str) -> Option<Equivocation> {
        let (x, y) = (Checkpoint::open(a).ok()?, Checkpoint::open(b).ok()?);
        (x.origin == y.origin && x.size == y.size && x.head != y.head).then(|| Equivocation {
            a: a.to_string(),
            b: b.to_string(),
        })
    }

    /// Re-check a proof received from someone else.
    #[must_use]
    pub fn verify(&self) -> bool {
        Self::detect(&self.a, &self.b).is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    BadCheckpoint(String),
    /// Smaller than a checkpoint already cosigned: the host is rolling the log back.
    Rollback {
        have: u64,
        offered: u64,
    },
    /// The host signed two different logs of the same size.
    Fork(Equivocation),
    /// The supplied entries do not extend the last view to this head.
    Inconsistent(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::BadCheckpoint(e) => write!(f, "bad checkpoint: {e}"),
            Refusal::Rollback { have, offered } => {
                write!(f, "rollback: already saw size {have}, offered {offered}")
            }
            Refusal::Fork(_) => write!(
                f,
                "fork: the log key signed two different heads for one size"
            ),
            Refusal::Inconsistent(e) => write!(f, "does not extend the last view: {e}"),
        }
    }
}

pub struct Witness {
    key: SigningKey,
    seen: BTreeMap<[u8; 32], Checkpoint>,
}

impl Witness {
    #[must_use]
    pub fn new(key: SigningKey) -> Self {
        Witness {
            key,
            seen: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn public(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// Cosign `checkpoint` given the entries that extend this witness's last view of the log (all of them,
    /// from genesis, the first time). Returns the cosignature text.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    ///
    /// # Panics
    ///
    /// Panics only if an internal invariant is broken, which indicates a bug.
    pub fn cosign(&mut self, checkpoint: &str, entries: &[Entry]) -> Result<String, Refusal> {
        let cp = Checkpoint::open(checkpoint).map_err(|e| Refusal::BadCheckpoint(e.to_string()))?;
        let last = self.seen.get(&cp.origin).cloned();
        let (base_size, base_head) = last.as_ref().map_or((0, [0; 32]), |l| (l.size, l.head));
        if cp.size < base_size {
            return Err(Refusal::Rollback {
                have: base_size,
                offered: cp.size,
            });
        }
        if cp.size == base_size {
            if cp.head != base_head {
                return Err(Refusal::Fork(Equivocation {
                    a: last.expect("size > 0 or head differs from zero").text,
                    b: cp.text,
                }));
            }
            if !entries.is_empty() {
                return Err(Refusal::Inconsistent(
                    "entries supplied for a checkpoint that adds none".into(),
                ));
            }
        } else {
            if entries.len() as u64 != cp.size - base_size {
                return Err(Refusal::Inconsistent(format!(
                    "{} entries supplied, {} needed",
                    entries.len(),
                    cp.size - base_size
                )));
            }
            let (mut prev, mut seq) = (base_head, base_size);
            for e in entries {
                if e.seq != seq || e.prev != prev {
                    return Err(Refusal::Inconsistent(format!(
                        "entry {} does not link to the last view",
                        e.seq
                    )));
                }
                if e.signer != cp.origin {
                    return Err(Refusal::Inconsistent(format!(
                        "entry {} is not signed by the log key",
                        e.seq
                    )));
                }
                e.check_signature()
                    .map_err(|err| Refusal::Inconsistent(err.to_string()))?;
                prev = e.id();
                seq += 1;
            }
            if prev != cp.head {
                return Err(Refusal::Inconsistent(
                    "entries do not end at the checkpoint's head".into(),
                ));
            }
        }
        let text = record::sign(
            COSIG,
            &[
                ("checkpoint", hex(&cp.digest())),
                ("origin", hex(&cp.origin)),
                ("size", cp.size.to_string()),
                ("head", hex(&cp.head)),
            ],
            &self.key,
        )
        .map_err(|e| Refusal::BadCheckpoint(e.to_string()))?;
        self.seen.insert(cp.origin, cp);
        Ok(text)
    }
}

/// A member's check: `checkpoint` is validly signed by the log key it names, and at least `m` distinct
/// keys from `witnesses` cosigned exactly this checkpoint.
///
/// # Errors
///
/// Returns a [`RecordError`] if the text or field is malformed, or its signature does not verify.
pub fn verify_quorum(
    checkpoint: &str,
    cosignatures: &[String],
    witnesses: &[[u8; 32]],
    m: usize,
) -> Result<Checkpoint, RecordError> {
    let cp = Checkpoint::open(checkpoint)?;
    let mut who = BTreeSet::new();
    for c in cosignatures {
        let Ok(r) = record::open(COSIG, c) else {
            continue;
        };
        if witnesses.contains(&r.signer)
            && r.req("checkpoint") == Ok(hex(&cp.digest()).as_str())
            && r.hash("head") == Ok(cp.head)
            && r.num("size") == Ok(cp.size)
        {
            who.insert(r.signer);
        }
    }
    if who.len() < m {
        return Err(format!(
            "{} of the required {m} witnesses cosigned this checkpoint",
            who.len()
        )
        .into());
    }
    Ok(cp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }
    const HOST: u8 = 1;

    fn grow(log: &mut Log, n: usize, tag: &str) {
        for i in 0..n {
            log.append(
                &k(HOST),
                "note",
                format!("{tag}{i}").as_bytes(),
                log.entries.len() as u64,
            );
        }
    }

    #[test]
    fn witnesses_cosign_growth_and_a_quorum_verifies() {
        let mut log = Log::new();
        grow(&mut log, 3, "a");
        let (mut w1, mut w2, mut w3) = (
            Witness::new(k(10)),
            Witness::new(k(11)),
            Witness::new(k(12)),
        );
        let cp1 = Checkpoint::sign(&log, &k(HOST), 100).unwrap();
        let sigs: Vec<String> = [&mut w1, &mut w2]
            .iter_mut()
            .map(|w| w.cosign(&cp1.text, &log.entries).unwrap())
            .collect();
        let pinned = [w1.public(), w2.public(), w3.public()];
        assert_eq!(verify_quorum(&cp1.text, &sigs, &pinned, 2).unwrap().size, 3);
        assert!(verify_quorum(&cp1.text, &sigs, &pinned, 3)
            .unwrap_err()
            .to_string()
            .contains("2 of the required 3"));
        // The log grows; witnesses need only the new entries.
        grow(&mut log, 2, "b");
        let cp2 = Checkpoint::sign(&log, &k(HOST), 200).unwrap();
        let more = w1.cosign(&cp2.text, &log.entries[3..]).unwrap();
        assert!(verify_quorum(&cp2.text, std::slice::from_ref(&more), &pinned, 1).is_ok());
        assert!(
            verify_quorum(&cp1.text, &[more], &pinned, 1).is_err(),
            "a cosignature is for one checkpoint"
        );
        // A witness that never saw the log needs all of it.
        assert!(w3.cosign(&cp2.text, &log.entries[3..]).is_err());
        w3.cosign(&cp2.text, &log.entries).unwrap();
    }

    #[test]
    fn duplicate_and_unpinned_witnesses_do_not_count_twice() {
        let mut log = Log::new();
        grow(&mut log, 1, "a");
        let mut w = Witness::new(k(10));
        let cp = Checkpoint::sign(&log, &k(HOST), 1).unwrap();
        let s = w.cosign(&cp.text, &log.entries).unwrap();
        assert!(
            verify_quorum(&cp.text, &[s.clone(), s.clone()], &[w.public()], 2).is_err(),
            "one witness twice is one witness"
        );
        assert!(
            verify_quorum(&cp.text, &[s], &[[9; 32]], 1).is_err(),
            "unpinned witness"
        );
    }

    #[test]
    fn a_split_view_is_caught_as_non_repudiable_equivocation() {
        let (mut log_a, mut log_b) = (Log::new(), Log::new());
        grow(&mut log_a, 2, "x");
        grow(&mut log_b, 2, "y"); // same size, different content
        let (cp_a, cp_b) = (
            Checkpoint::sign(&log_a, &k(HOST), 1).unwrap(),
            Checkpoint::sign(&log_b, &k(HOST), 1).unwrap(),
        );
        let mut w = Witness::new(k(10));
        w.cosign(&cp_a.text, &log_a.entries).unwrap();
        let Err(Refusal::Fork(proof)) = w.cosign(&cp_b.text, &[]) else {
            panic!("should detect the fork")
        };
        assert!(proof.verify(), "anyone can re-check the proof");
        assert!(Equivocation::detect(&cp_a.text, &cp_a.text).is_none());
        assert!(Equivocation::detect(&cp_a.text, &cp_b.text).is_some());
    }

    #[test]
    fn rollbacks_and_non_extending_entries_are_refused() {
        let mut log = Log::new();
        grow(&mut log, 4, "a");
        let mut w = Witness::new(k(10));
        let cp4 = Checkpoint::sign(&log, &k(HOST), 1).unwrap();
        w.cosign(&cp4.text, &log.entries).unwrap();
        let short = Log {
            entries: log.entries[..2].to_vec(),
        };
        let cp2 = Checkpoint::sign(&short, &k(HOST), 2).unwrap();
        assert_eq!(
            w.cosign(&cp2.text, &[]).unwrap_err(),
            Refusal::Rollback {
                have: 4,
                offered: 2
            }
        );

        // A rewritten history that reaches a larger size but does not extend what the witness saw.
        let mut other = Log::new();
        grow(&mut other, 6, "z");
        let cp6 = Checkpoint::sign(&other, &k(HOST), 3).unwrap();
        assert!(matches!(
            w.cosign(&cp6.text, &other.entries[4..]),
            Err(Refusal::Inconsistent(_))
        ));
    }

    #[test]
    fn entries_signed_by_another_key_or_ending_elsewhere_are_refused() {
        let mut log = Log::new();
        grow(&mut log, 2, "a");
        let cp = Checkpoint::sign(&log, &k(HOST), 1).unwrap();
        let mut w = Witness::new(k(10));
        let mut forged = log.entries.clone();
        forged[1].sig[0] ^= 1;
        assert!(matches!(
            w.cosign(&cp.text, &forged),
            Err(Refusal::Inconsistent(_))
        ));
        assert!(matches!(
            w.cosign(&cp.text, &log.entries[..1]),
            Err(Refusal::Inconsistent(_))
        ));
        let mut foreign = Log::new();
        foreign.append(&k(77), "note", b"x", 1);
        let cp_foreign = Checkpoint::sign(&foreign, &k(HOST), 1).unwrap();
        assert!(
            matches!(
                w.cosign(&cp_foreign.text, &foreign.entries),
                Err(Refusal::Inconsistent(_))
            ),
            "entries must be signed by the checkpoint's key"
        );
        assert!(matches!(
            w.cosign("garbage", &[]),
            Err(Refusal::BadCheckpoint(_))
        ));
    }
}
