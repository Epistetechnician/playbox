//! A room's replicated, signed log (host-less rooms, milestone M1; see `docs/host-less-rooms.md`).
//!
//! One host, the **leader**, sequences entries. Every other host of the room, a **follower**, stores and checks
//! each entry as it arrives. The leader's say-so is not enough for an entry to count: it counts as
//! **committed** once `k` distinct hosts have cosigned a [`Checkpoint`] that covers it, after replaying the
//! log up to that point themselves.
//!
//! - The log is bound to a room by the room's root public key, which is inside every signature, so an entry
//!   or checkpoint from one room cannot be replayed into another.
//! - Entry 0 is the **genesis**: kind `epoch`, signed by the room's *root* key, naming the leader, the host
//!   set and the threshold `k`. A follower that knows only the root public key can therefore trust it.
//! - Every later entry is signed by the leader named in the genesis. Followers refuse anything else.
//! - Membership is a log entry (`member.join`, `member.revoke`, `member.leave`), so every host replaying the
//!   log derives the same roster.
//!
//! - **Failover (M2).** A new leader takes over with an `epoch` entry carrying `k` host signatures over a
//!   [`Proposal`]. A host signs a proposal only after replaying the proposer's log, finding in it the last
//!   checkpoint the host cosigned (so nothing committed is lost), and never having promised a higher epoch (so
//!   two leaders cannot both reach `k` in one epoch). A follower holding an uncommitted tail from the old
//!   leader drops it when the epoch entry arrives.
//! - **Equivocation (M2).** Two different entries signed by one key at one `(epoch, index)` are an
//!   [`Equivocation`]: a self-contained proof, checked with the room key alone.
//!
//! - **Host-set change and key rotation (M3).** An epoch entry may name a different host set and threshold. It
//!   then needs two quorums over the same [`Proposal`]: `k` hosts of the old set (who authorize the change) and
//!   `k` of the new set (who show they hold the log and accept the job), so no schedule leaves two groups each
//!   able to commit. Rotating a host's key is removing the old key and adding the new one. A removed host's
//!   cosignatures still count for the epochs it was in and for nothing after.

use crate::framing::be_len32;
use crate::promotion;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const ENTRY_DOMAIN: &[u8] = b"playbox.roomlog.entry.v1";
const CHECKPOINT_DOMAIN: &[u8] = b"playbox.roomlog.checkpoint.v1";
const CHECKPOINT_PQ_CTX: &[u8] = b"playbox.roomlog.checkpoint.pq.v1";

/// Most bytes one entry's payload may hold.
pub const MAX_PAYLOAD: usize = 16 * 1024;
/// Most hosts a room may name.
pub const MAX_HOSTS: usize = 16;

pub type Key = [u8; 32];
pub type Hash = [u8; 32];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LogError {
    #[error("entry {0}: not the next entry (expected index {1})")]
    BadIndex(u64, u64),
    #[error("entry {0}: previous hash does not match")]
    BadPrev(u64),
    #[error("entry {0}: signature does not verify")]
    BadSignature(u64),
    #[error("entry {0}: signed by a key that is not the leader")]
    NotLeader(u64),
    #[error("entry {0}: the genesis must be signed by the room's root key")]
    BadGenesis(u64),
    #[error("entry {0}: time goes backwards")]
    TimeBackwards(u64),
    #[error("entry {0}: the epoch change has fewer than k host signatures")]
    NoQuorum(u64),
    #[error("entry {0}: {1}")]
    Promotion(u64, String),
    #[error("proposal: {0}")]
    BadProposal(String),
    #[error("not an equivocation: {0}")]
    NotEquivocation(String),
    #[error("entry {0}: unknown or malformed {1}")]
    Malformed(u64, String),
    #[error("entry {0}: payload is larger than {MAX_PAYLOAD} bytes")]
    TooLarge(u64),
    #[error("the log is empty")]
    Empty,
    #[error("host set: {0}")]
    BadConfig(String),
    #[error("this key is not the leader")]
    WrongKey,
    #[error("checkpoint: {0}")]
    BadCheckpoint(String),
}

/// Who runs the room and how many of them must agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub epoch: u64,
    pub leader: Key,
    pub hosts: BTreeSet<Key>,
    /// Cosignatures needed to commit. Always more than half the hosts, so two disjoint groups cannot both commit.
    pub k: usize,
}

impl Config {
    /// # Errors
    ///
    /// Fails if the leader is not a host, there are too many or no hosts, or `k` is not more than half and at most all of them.
    pub fn check(&self) -> Result<(), LogError> {
        let n = self.hosts.len();
        if n == 0 || n > MAX_HOSTS {
            return Err(LogError::BadConfig(format!(
                "{n} hosts; need 1 to {MAX_HOSTS}"
            )));
        }
        if !self.hosts.contains(&self.leader) {
            return Err(LogError::BadConfig(
                "the leader is not in the host set".into(),
            ));
        }
        if self.k > n || self.k * 2 <= n {
            return Err(LogError::BadConfig(format!(
                "threshold {} must be more than half of {n} hosts and at most {n}",
                self.k
            )));
        }
        Ok(())
    }

    fn to_payload(&self) -> Vec<u8> {
        json!({
            "epoch": self.epoch,
            "leader": hex(&self.leader),
            "hosts": self.hosts.iter().map(|h| hex(h)).collect::<Vec<_>>(),
            "k": self.k,
        })
        .to_string()
        .into_bytes()
    }

    fn from_payload(index: u64, p: &[u8]) -> Result<Self, LogError> {
        let bad = |m: &str| LogError::Malformed(index, format!("epoch ({m})"));
        let v: Value = serde_json::from_slice(p).map_err(|_| bad("not json"))?;
        let epoch = v["epoch"].as_u64().ok_or_else(|| bad("epoch"))?;
        let leader = unhex(v["leader"].as_str().ok_or_else(|| bad("leader"))?)
            .ok_or_else(|| bad("leader"))?;
        let mut hosts = BTreeSet::new();
        for h in v["hosts"].as_array().ok_or_else(|| bad("hosts"))? {
            hosts.insert(unhex(h.as_str().ok_or_else(|| bad("host"))?).ok_or_else(|| bad("host"))?);
        }
        let k = usize::try_from(v["k"].as_u64().ok_or_else(|| bad("k"))?).map_err(|_| bad("k"))?;
        let c = Self {
            epoch,
            leader,
            hosts,
            k,
        };
        c.check()?;
        Ok(c)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub epoch: u64,
    pub index: u64,
    pub prev: Hash,
    pub kind: String,
    pub payload: Vec<u8>,
    pub time_ms: u64,
    pub signer: Key,
    pub sig: [u8; 64],
}

impl Entry {
    fn signing_bytes(&self, room: &Key) -> Vec<u8> {
        let mut b =
            Vec::with_capacity(ENTRY_DOMAIN.len() + 160 + self.kind.len() + self.payload.len());
        b.extend_from_slice(ENTRY_DOMAIN);
        b.extend_from_slice(room);
        b.extend_from_slice(&self.epoch.to_be_bytes());
        b.extend_from_slice(&self.index.to_be_bytes());
        b.extend_from_slice(&self.prev);
        b.extend_from_slice(&be_len32(self.kind.len()));
        b.extend_from_slice(self.kind.as_bytes());
        b.extend_from_slice(&be_len32(self.payload.len()));
        b.extend_from_slice(&self.payload);
        b.extend_from_slice(&self.time_ms.to_be_bytes());
        b.extend_from_slice(&self.signer);
        b
    }

    /// Identity of the entry, covering the signature.
    #[must_use]
    pub fn id(&self, room: &Key) -> Hash {
        let mut h = blake3::Hasher::new();
        h.update(&self.signing_bytes(room));
        h.update(&self.sig);
        *h.finalize().as_bytes()
    }

    fn sign(mut self, room: &Key, key: &SigningKey) -> Self {
        self.sig = key.sign(&self.signing_bytes(room)).to_bytes();
        self
    }

    fn check_signature(&self, room: &Key) -> Result<(), LogError> {
        let k = VerifyingKey::from_bytes(&self.signer)
            .map_err(|_| LogError::BadSignature(self.index))?;
        k.verify(&self.signing_bytes(room), &Signature::from_bytes(&self.sig))
            .map_err(|_| LogError::BadSignature(self.index))
    }
}

/// A statement that the log of this room, in this epoch, has `size` entries ending in `head`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub epoch: u64,
    pub size: u64,
    pub head: Hash,
}

impl Checkpoint {
    fn signing_bytes(&self, room: &Key) -> Vec<u8> {
        let mut b = Vec::with_capacity(CHECKPOINT_DOMAIN.len() + 80);
        b.extend_from_slice(CHECKPOINT_DOMAIN);
        b.extend_from_slice(room);
        b.extend_from_slice(&self.epoch.to_be_bytes());
        b.extend_from_slice(&self.size.to_be_bytes());
        b.extend_from_slice(&self.head);
        b
    }

    fn pq_signing_bytes(&self, room: &Key, signer: &Key) -> Vec<u8> {
        let mut b = self.signing_bytes(room);
        b.extend_from_slice(signer);
        b
    }
}

/// Stateless FIPS 204 ML-DSA-65 cosignature attached to a [`SignedCheckpoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PqSig {
    pub pk: Vec<u8>,
    pub sig: Vec<u8>,
}

/// A checkpoint with the cosignatures collected for it, by host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedCheckpoint {
    pub cp: Checkpoint,
    pub sigs: BTreeMap<Key, [u8; 64]>,
    pub pq_sigs: BTreeMap<Key, PqSig>,
}

impl SignedCheckpoint {
    #[must_use]
    pub fn new(cp: Checkpoint) -> Self {
        Self {
            cp,
            sigs: BTreeMap::new(),
            pq_sigs: BTreeMap::new(),
        }
    }

    /// Add `signer`'s signature if it is a host of `cfg` and verifies. Returns whether it was new.
    pub fn add(&mut self, room: &Key, cfg: &Config, signer: Key, sig: [u8; 64]) -> bool {
        if !cfg.hosts.contains(&signer) || self.sigs.contains_key(&signer) {
            return false;
        }
        let ok = VerifyingKey::from_bytes(&signer)
            .and_then(|k| k.verify(&self.cp.signing_bytes(room), &Signature::from_bytes(&sig)))
            .is_ok();
        if ok {
            self.sigs.insert(signer, sig);
        }
        ok
    }

    /// Add `signer`'s ML-DSA-65 cosignature if `signer` is a host of `cfg` and it verifies.
    pub fn add_pq(&mut self, room: &Key, cfg: &Config, signer: Key, pq: PqSig) -> bool {
        if !cfg.hosts.contains(&signer) || self.pq_sigs.contains_key(&signer) {
            return false;
        }
        let msg = self.cp.pq_signing_bytes(room, &signer);
        let ok = crate::pq::verify_mldsa65(&pq.pk, &msg, &pq.sig, CHECKPOINT_PQ_CTX);
        if ok {
            self.pq_sigs.insert(signer, pq);
        }
        ok
    }

    /// Valid distinct host cosignatures.
    #[must_use]
    pub fn count(&self, room: &Key, cfg: &Config) -> usize {
        self.sigs
            .iter()
            .filter(|(s, sig)| {
                cfg.hosts.contains(*s)
                    && VerifyingKey::from_bytes(s)
                        .and_then(|k| {
                            k.verify(&self.cp.signing_bytes(room), &Signature::from_bytes(sig))
                        })
                        .is_ok()
            })
            .count()
    }

    /// Valid distinct host cosignatures carrying BOTH valid Ed25519 and valid ML-DSA-65 signatures.
    #[must_use]
    pub fn pq_count(&self, room: &Key, cfg: &Config) -> usize {
        self.sigs
            .iter()
            .filter(|(s, sig)| {
                if !cfg.hosts.contains(*s) {
                    return false;
                }
                let ed_ok = VerifyingKey::from_bytes(s)
                    .and_then(|k| {
                        k.verify(&self.cp.signing_bytes(room), &Signature::from_bytes(sig))
                    })
                    .is_ok();
                if !ed_ok {
                    return false;
                }
                let Some(pq) = self.pq_sigs.get(*s) else {
                    return false;
                };
                crate::pq::verify_mldsa65(
                    &pq.pk,
                    &self.cp.pq_signing_bytes(room, s),
                    &pq.sig,
                    CHECKPOINT_PQ_CTX,
                )
            })
            .count()
    }
}

const PROPOSAL_DOMAIN: &[u8] = b"playbox.roomlog.proposal.v1";

/// A host's bid to lead epoch `epoch`, starting its epoch entry at position `size` after the entry whose id is `head`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub epoch: u64,
    pub leader: Key,
    pub size: u64,
    pub head: Hash,
    /// The hosts and threshold of the epoch being proposed (the current ones unless the bid changes them).
    pub hosts: BTreeSet<Key>,
    pub k: usize,
}

impl Proposal {
    fn signing_bytes(&self, room: &Key) -> Vec<u8> {
        let mut b = Vec::with_capacity(PROPOSAL_DOMAIN.len() + 144 + 32 * self.hosts.len());
        b.extend_from_slice(PROPOSAL_DOMAIN);
        b.extend_from_slice(room);
        b.extend_from_slice(&self.epoch.to_be_bytes());
        b.extend_from_slice(&self.leader);
        b.extend_from_slice(&self.size.to_be_bytes());
        b.extend_from_slice(&self.head);
        b.extend_from_slice(&(self.k as u64).to_be_bytes());
        b.extend_from_slice(&(self.hosts.len() as u64).to_be_bytes());
        for h in &self.hosts {
            b.extend_from_slice(h);
        }
        b
    }

    /// Valid signatures from distinct hosts of `cfg`.
    fn count(&self, room: &Key, cfg: &Config, sigs: &BTreeMap<Key, [u8; 64]>) -> usize {
        let msg = self.signing_bytes(room);
        sigs.iter()
            .filter(|(s, sig)| {
                cfg.hosts.contains(*s)
                    && VerifyingKey::from_bytes(s)
                        .and_then(|k| k.verify(&msg, &Signature::from_bytes(sig)))
                        .is_ok()
            })
            .count()
    }
}

/// Proof that one key signed two different entries at the same epoch and index. It checks with the room key
/// alone, so any member can verify it offline. Whether the key is a host of the room is for the caller to see
/// in the log it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Equivocation {
    pub a: Entry,
    pub b: Entry,
}

impl Equivocation {
    /// The key that signed both.
    ///
    /// # Errors
    ///
    /// Fails unless both entries are validly signed by one key for the same epoch and index and differ.
    pub fn verify(&self, room: &Key) -> Result<Key, LogError> {
        let no = |m: &str| LogError::NotEquivocation(m.to_string());
        if self.a.signer != self.b.signer {
            return Err(no("different signers"));
        }
        if (self.a.epoch, self.a.index) != (self.b.epoch, self.b.index) {
            return Err(no("different epoch or index"));
        }
        if self.a.signing_bytes(room) == self.b.signing_bytes(room) {
            return Err(no("the same entry twice"));
        }
        self.a.check_signature(room)?;
        self.b.check_signature(room)?;
        Ok(self.a.signer)
    }
}

/// The log, held by every host of the room.
#[derive(Debug, Clone)]
pub struct RoomLog {
    room: Key,
    entries: Vec<Entry>,
    cfg: Config,
    /// The configuration of each epoch, in order, and the index of the entry that began it.
    hist: Vec<Config>,
    starts: Vec<u64>,
}

/// The roster a log replays to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roster {
    /// Member endpoint ids (as the host prints them) currently in the room.
    pub members: BTreeSet<String>,
    /// Members barred from rejoining.
    pub banned: BTreeSet<String>,
}

impl RoomLog {
    /// Start a room's log: the genesis entry, signed by the room's root key.
    ///
    /// # Errors
    ///
    /// Fails if `cfg` is not a valid host set, or `root` is not the room's key.
    pub fn genesis(root: &SigningKey, cfg: Config, time_ms: u64) -> Result<Self, LogError> {
        cfg.check()?;
        let room = root.verifying_key().to_bytes();
        let e = Entry {
            epoch: cfg.epoch,
            index: 0,
            prev: [0; 32],
            kind: "epoch".into(),
            payload: cfg.to_payload(),
            time_ms,
            signer: room,
            sig: [0; 64],
        }
        .sign(&room, root);
        Ok(Self {
            room,
            entries: vec![e],
            hist: vec![cfg.clone()],
            starts: vec![0],
            cfg,
        })
    }

    /// A log with no entries yet, for a follower that knows only the room's root public key.
    #[must_use]
    pub fn follower(room: Key) -> FollowerLog {
        FollowerLog { room, log: None }
    }

    #[must_use]
    pub fn room(&self) -> Key {
        self.room
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        self.entries.len() as u64
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The id of the last entry.
    #[must_use]
    pub fn head(&self) -> Hash {
        self.head_at(self.len()).unwrap_or([0; 32])
    }

    /// The id of the entry before position `size` (the head of a log of that size). `None` past the end.
    #[must_use]
    pub fn head_at(&self, size: u64) -> Option<Hash> {
        if size == 0 {
            return Some([0; 32]);
        }
        self.entries
            .get(usize::try_from(size - 1).ok()?)
            .map(|e| e.id(&self.room))
    }

    fn check_next(&self, e: &Entry) -> Result<(), LogError> {
        let want = self.len();
        if e.index != want {
            return Err(LogError::BadIndex(e.index, want));
        }
        if e.prev != self.head() {
            return Err(LogError::BadPrev(e.index));
        }
        if e.payload.len() > MAX_PAYLOAD {
            return Err(LogError::TooLarge(e.index));
        }
        if self.entries.last().is_some_and(|l| e.time_ms < l.time_ms) {
            return Err(LogError::TimeBackwards(e.index));
        }
        if e.kind == "epoch" {
            return self.check_epoch(e);
        }
        if e.epoch != self.cfg.epoch {
            return Err(LogError::Malformed(e.index, "epoch".into()));
        }
        if e.signer != self.cfg.leader {
            return Err(LogError::NotLeader(e.index));
        }
        e.check_signature(&self.room)?;
        validate_event(e)?;
        match e.kind.as_str() {
            "problem.register" => {
                promotion::check_register(e, &self.cfg.hosts, &promotion::replay(&self.entries))
            }
            "promotion" => promotion::check_promotion(
                &self.room,
                e,
                &self.cfg.hosts,
                &promotion::replay(&self.entries),
            ),
            _ => Ok(()),
        }
    }

    /// An epoch change: a later epoch number, signed by the new leader, and carrying signatures over the matching
    /// [`Proposal`]: `k` from hosts of the current epoch and, if the host set or threshold changes, `k` from the new
    /// set too (a host in both sets counts in both).
    fn check_epoch(&self, e: &Entry) -> Result<(), LogError> {
        let new = Config::from_payload(e.index, &e.payload)?;
        if e.epoch <= self.cfg.epoch || new.epoch != e.epoch {
            return Err(LogError::Malformed(e.index, "epoch number".into()));
        }
        if e.signer != new.leader {
            return Err(LogError::NotLeader(e.index));
        }
        e.check_signature(&self.room)?;
        let sigs = epoch_sigs(e)?;
        let p = Proposal {
            epoch: new.epoch,
            leader: new.leader,
            size: e.index,
            head: e.prev,
            hosts: new.hosts.clone(),
            k: new.k,
        };
        if p.count(&self.room, &self.cfg, &sigs) < self.cfg.k
            || p.count(&self.room, &new, &sigs) < new.k
        {
            return Err(LogError::NoQuorum(e.index));
        }
        Ok(())
    }

    fn push(&mut self, e: Entry) {
        if e.kind == "epoch" {
            if let Ok(c) = Config::from_payload(e.index, &e.payload) {
                self.cfg = c.clone();
                self.hist.push(c);
                self.starts.push(e.index);
            }
        }
        self.entries.push(e);
    }

    /// Rebuild from the first `n` entries (dropping the rest), replaying every check.
    fn truncated(&self, n: usize) -> Result<Self, LogError> {
        let mut it = self.entries.iter().take(n);
        let g = it.next().ok_or(LogError::Empty)?;
        let mut fresh = verify_genesis(&self.room, g)?;
        for e in it {
            fresh.accept(e.clone())?;
        }
        Ok(fresh)
    }

    /// Leader only: append an event signed by the leader's key.
    ///
    /// # Errors
    ///
    /// Fails if `key` is not the leader, the kind is unknown or the payload is malformed.
    pub fn append(
        &mut self,
        key: &SigningKey,
        kind: &str,
        payload: &Value,
        time_ms: u64,
    ) -> Result<&Entry, LogError> {
        if key.verifying_key().to_bytes() != self.cfg.leader {
            return Err(LogError::WrongKey);
        }
        let time_ms = time_ms.max(self.entries.last().map_or(0, |l| l.time_ms));
        let e = Entry {
            epoch: self.cfg.epoch,
            index: self.len(),
            prev: self.head(),
            kind: kind.to_string(),
            payload: payload.to_string().into_bytes(),
            time_ms,
            signer: self.cfg.leader,
            sig: [0; 64],
        }
        .sign(&self.room, key);
        self.check_next(&e)?;
        self.push(e);
        Ok(self.entries.last().expect("just pushed"))
    }

    /// Follower: accept the next entry from the leader, after checking all of it.
    ///
    /// # Errors
    ///
    /// Fails, and changes nothing, if the entry is not exactly the next valid entry.
    pub fn accept(&mut self, e: Entry) -> Result<(), LogError> {
        self.check_next(&e)?;
        self.push(e);
        Ok(())
    }

    /// Follower: accept entries in order, skipping any this log already holds. Stops at the first bad one.
    ///
    /// # Errors
    ///
    /// The first entry that does not check.
    pub fn accept_all(&mut self, entries: Vec<Entry>) -> Result<usize, LogError> {
        let mut n = 0;
        for e in entries {
            if self.take(e)? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// One entry from a peer: `Ok(false)` if already held, `Ok(true)` if added (an epoch entry may replace the tail).
    fn take(&mut self, e: Entry) -> Result<bool, LogError> {
        if e.index < self.len() {
            if self.entries[usize::try_from(e.index).unwrap_or(usize::MAX)] == e {
                return Ok(false);
            }
            if e.kind != "epoch" {
                return Err(LogError::BadPrev(e.index));
            }
            self.accept_epoch(e)?;
        } else if e.kind == "epoch" {
            self.accept_epoch(e)?;
        } else {
            self.accept(e)?;
        }
        Ok(true)
    }

    /// Follower: take an epoch entry even if it replaces entries this host holds past its `prev`. The entry's
    /// quorum signatures are what allow dropping them: `k` hosts each checked that the new leader's log holds
    /// everything they cosigned, so nothing committed is in the part dropped.
    ///
    /// # Errors
    ///
    /// Fails, and changes nothing, if the entry does not check against the log up to its own index.
    pub fn accept_epoch(&mut self, e: Entry) -> Result<(), LogError> {
        let at = usize::try_from(e.index).unwrap_or(usize::MAX);
        if at >= self.entries.len() {
            return self.accept(e);
        }
        if self.head_at(e.index) != Some(e.prev) {
            return Err(LogError::BadPrev(e.index));
        }
        let mut t = self.truncated(at)?;
        t.accept(e)?;
        *self = t;
        Ok(())
    }

    /// An [`Equivocation`] between `e` and the entry this log holds at the same position, if there is one.
    #[must_use]
    pub fn equivocation_with(&self, e: &Entry) -> Option<Equivocation> {
        let held = self.entries.get(usize::try_from(e.index).ok()?)?;
        let proof = Equivocation {
            a: held.clone(),
            b: e.clone(),
        };
        proof.verify(&self.room).ok().map(|_| proof)
    }

    /// The first equivocation among `incoming` against this log, if any.
    #[must_use]
    pub fn find_equivocation(&self, incoming: &[Entry]) -> Option<Equivocation> {
        incoming.iter().find_map(|e| self.equivocation_with(e))
    }

    /// The proposal this host would make to lead the next epoch from the log as it stands.
    #[must_use]
    pub fn proposal(&self, me: Key) -> Proposal {
        self.proposal_at(me, self.cfg.epoch + 1)
    }

    /// A proposal for a later epoch than the next: a host whose bid failed bids higher, since hosts that signed
    /// a rival bid for the lower one will not sign again at it. Epoch numbers only have to increase.
    #[must_use]
    pub fn proposal_at(&self, me: Key, epoch: u64) -> Proposal {
        self.proposal_for(me, epoch, self.cfg.hosts.clone(), self.cfg.k)
    }

    /// A proposal that also changes the host set and threshold. `me` may be the current leader: it asks for the
    /// change instead of for a handover.
    #[must_use]
    pub fn proposal_for(&self, me: Key, epoch: u64, hosts: BTreeSet<Key>, k: usize) -> Proposal {
        Proposal {
            epoch: epoch.max(self.cfg.epoch + 1),
            leader: me,
            size: self.len(),
            head: self.head(),
            hosts,
            k,
        }
    }

    /// Sign `p` for its proposer's log `theirs`, if this host can see that nothing it relies on is lost and no
    /// other leader holds its promise. `last` is the latest checkpoint this host cosigned and `promised` the
    /// highest epoch it has signed a proposal for. The caller records `p.epoch` as promised before sending the signature.
    ///
    /// # Errors
    ///
    /// Fails if `key` is not a host, `p` does not match `theirs`, `theirs` is behind this host's epoch or lacks
    /// the last checkpoint this host cosigned, or this host promised `p.epoch` or higher to a different bid.
    pub fn cosign_proposal(
        &self,
        key: &SigningKey,
        theirs: &RoomLog,
        p: &Proposal,
        last: Option<&Checkpoint>,
        promised: Option<&Proposal>,
    ) -> Result<(Key, [u8; 64]), LogError> {
        let bad = |m: &str| LogError::BadProposal(m.to_string());
        let me = key.verifying_key().to_bytes();
        if !(self.cfg.hosts.contains(&me) || p.hosts.contains(&me)) || theirs.room != self.room {
            return Err(LogError::WrongKey);
        }
        if p.epoch <= theirs.cfg.epoch || p.size != theirs.len() || p.head != theirs.head() {
            return Err(bad("does not match the proposer's log"));
        }
        Config {
            epoch: p.epoch,
            leader: p.leader,
            hosts: p.hosts.clone(),
            k: p.k,
        }
        .check()
        .map_err(|e| bad(&e.to_string()))?;
        let reconfig = p.hosts != theirs.cfg.hosts || p.k != theirs.cfg.k;
        if !reconfig && p.leader == theirs.cfg.leader {
            return Err(bad("the proposer already leads and changes nothing"));
        }
        if theirs.cfg.epoch < self.cfg.epoch {
            return Err(bad(
                "the proposer's log is in an older epoch than this host's",
            ));
        }
        if let Some(c) = last {
            if theirs.head_at(c.size) != Some(c.head) {
                return Err(bad(
                    "the proposer's log lacks a checkpoint this host cosigned",
                ));
            }
        }
        if let Some(q) = promised {
            if q.epoch > p.epoch || (q.epoch == p.epoch && q != p) {
                return Err(bad(
                    "this host promised that epoch or a later one to another bid",
                ));
            }
        }
        Ok((me, key.sign(&p.signing_bytes(&self.room)).to_bytes()))
    }

    /// Leader-elect: append the epoch entry that makes `p.leader` the leader, from `sigs` (hosts' signatures over `p`).
    ///
    /// # Errors
    ///
    /// Fails if `key` is not `p.leader`, `p` is not for this log's head, or fewer than `k` signatures verify.
    pub fn new_epoch(
        &mut self,
        key: &SigningKey,
        p: &Proposal,
        sigs: &BTreeMap<Key, [u8; 64]>,
        time_ms: u64,
    ) -> Result<&Entry, LogError> {
        if key.verifying_key().to_bytes() != p.leader
            || *p != self.proposal_for(p.leader, p.epoch, p.hosts.clone(), p.k)
        {
            return Err(LogError::WrongKey);
        }
        let cfg = Config {
            epoch: p.epoch,
            leader: p.leader,
            hosts: p.hosts.clone(),
            k: p.k,
        };
        cfg.check()?;
        let mut v: Value = serde_json::from_slice(&cfg.to_payload()).expect("own json");
        v["sigs"] = sigs
            .iter()
            .map(|(k, s)| (hex(k), Value::from(hex(s))))
            .collect::<serde_json::Map<_, _>>()
            .into();
        let e = Entry {
            epoch: p.epoch,
            index: self.len(),
            prev: self.head(),
            kind: "epoch".into(),
            payload: v.to_string().into_bytes(),
            time_ms: time_ms.max(self.entries.last().map_or(0, |l| l.time_ms)),
            signer: p.leader,
            sig: [0; 64],
        }
        .sign(&self.room, key);
        self.check_next(&e)?;
        self.push(e);
        Ok(self.entries.last().expect("just pushed"))
    }

    /// Check the whole log from the genesis. Returns the host set it names.
    ///
    /// # Errors
    ///
    /// Fails at the first entry that does not check.
    pub fn verify(&self) -> Result<Config, LogError> {
        let mut it = self.entries.iter();
        let g = it.next().ok_or(LogError::Empty)?;
        let mut fresh = verify_genesis(&self.room, g)?;
        for e in it {
            fresh.accept(e.clone())?;
        }
        Ok(fresh.cfg)
    }

    /// Register a challenge problem and its promotion rule (leader only). `j` is the number of distinct hosts a
    /// promotion needs, at least 2.
    ///
    /// # Errors
    ///
    /// Fails if `key` is not the leader or the registration breaks the rules.
    pub fn register_problem(
        &mut self,
        key: &SigningKey,
        reg: &promotion::Register,
        time_ms: u64,
    ) -> Result<&Entry, LogError> {
        self.append(
            key,
            "problem.register",
            &promotion::register_payload(reg),
            time_ms,
        )
    }

    /// Record a promotion (leader only), carrying the receipts. Every host replaying the log re-checks them.
    ///
    /// # Errors
    ///
    /// Fails if `key` is not the leader or the promotion does not meet its problem's rule.
    pub fn promote(
        &mut self,
        key: &SigningKey,
        p: &promotion::Promotion,
        time_ms: u64,
    ) -> Result<&Entry, LogError> {
        self.append(key, "promotion", &promotion::promotion_payload(p), time_ms)
    }

    /// The frontier of each registered problem. With `certs`, only what is committed; without, everything the log holds.
    #[must_use]
    pub fn frontier(&self, committed_by: Option<&[SignedCheckpoint]>) -> promotion::State {
        let n = committed_by.map_or(self.entries.len(), |c| {
            usize::try_from(self.committed(c)).unwrap_or(usize::MAX)
        });
        promotion::replay(&self.entries[..n.min(self.entries.len())])
    }

    /// Replay membership events.
    #[must_use]
    pub fn roster(&self) -> Roster {
        let mut r = Roster::default();
        for e in &self.entries {
            let Ok(v) = serde_json::from_slice::<Value>(&e.payload) else {
                continue;
            };
            let Some(m) = v["member"].as_str() else {
                continue;
            };
            match e.kind.as_str() {
                "member.join" if !r.banned.contains(m) => {
                    r.members.insert(m.to_string());
                }
                "member.leave" => {
                    r.members.remove(m);
                }
                "member.revoke" => {
                    r.members.remove(m);
                    r.banned.insert(m.to_string());
                }
                _ => {}
            }
        }
        r
    }

    /// A checkpoint of the whole log as it stands.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            epoch: self.cfg.epoch,
            size: self.len(),
            head: self.head(),
        }
    }

    /// Cosign `cp`, but only after checking it against this host's own copy of the log: same epoch, a size
    /// this host has reached, and the head this host computed for that size. A host never signs what it has not replayed.
    ///
    /// # Errors
    ///
    /// Fails if the checkpoint does not match this log, or `key` is not a host of the room.
    pub fn cosign(&self, key: &SigningKey, cp: &Checkpoint) -> Result<(Key, [u8; 64]), LogError> {
        let me = key.verifying_key().to_bytes();
        if !self.cfg.hosts.contains(&me) {
            return Err(LogError::WrongKey);
        }
        if cp.epoch != self.cfg.epoch {
            return Err(LogError::BadCheckpoint("wrong epoch".into()));
        }
        match self.head_at(cp.size) {
            Some(h) if h == cp.head && cp.size > 0 => {}
            Some(_) => {
                return Err(LogError::BadCheckpoint(
                    "head differs from this host's log".into(),
                ))
            }
            None => {
                return Err(LogError::BadCheckpoint(
                    "this host has not reached that size".into(),
                ))
            }
        }
        Ok((me, key.sign(&cp.signing_bytes(&self.room)).to_bytes()))
    }

    /// Cosign `cp` with both Ed25519 and deterministic ML-DSA-65 derived from `key`'s seed.
    ///
    /// # Errors
    ///
    /// As [`RoomLog::cosign`], or if ML-DSA-65 signing fails.
    pub fn cosign_hybrid(
        &self,
        key: &SigningKey,
        cp: &Checkpoint,
    ) -> Result<(Key, [u8; 64], PqSig), LogError> {
        let (me, sig) = self.cosign(key, cp)?;
        let (pq_pk, pq_sk) = crate::pq::derive_mldsa65_from_ed25519_seed(&key.to_bytes());
        let pq_sig = crate::pq::sign_mldsa65(
            &pq_sk,
            &cp.pq_signing_bytes(&self.room, &me),
            CHECKPOINT_PQ_CTX,
        )
        .map_err(|e| LogError::BadCheckpoint(e.to_string()))?;
        Ok((
            me,
            sig,
            PqSig {
                pk: pq_pk,
                sig: pq_sig,
            },
        ))
    }

    /// Make a [`SignedCheckpoint`] for `cp` carrying `key`'s own hybrid (Ed25519 + ML-DSA-65) cosignature.
    ///
    /// # Errors
    ///
    /// As [`RoomLog::cosign_hybrid`].
    pub fn sign_checkpoint(
        &self,
        key: &SigningKey,
        cp: Checkpoint,
    ) -> Result<SignedCheckpoint, LogError> {
        let (me, sig, pq) = self.cosign_hybrid(key, &cp)?;
        let mut sc = SignedCheckpoint::new(cp);
        sc.add(&self.room, &self.cfg, me, sig);
        sc.add_pq(&self.room, &self.cfg, me, pq);
        Ok(sc)
    }

    /// Merge `other`'s signatures into `into` if it is the same checkpoint and the signatures verify. Returns how many were new.
    pub fn merge(&self, into: &mut SignedCheckpoint, other: &SignedCheckpoint) -> usize {
        let Some(cfg) = self.hist.iter().find(|c| c.epoch == into.cp.epoch) else {
            return 0;
        };
        if into.cp != other.cp {
            return 0;
        }
        let ed = other
            .sigs
            .iter()
            .filter(|(s, sig)| into.add(&self.room, cfg, **s, **sig))
            .count();
        let pq = other
            .pq_sigs
            .iter()
            .filter(|(s, pq)| into.add_pq(&self.room, cfg, **s, (*pq).clone()))
            .count();
        ed + pq
    }

    /// How many entries are committed: the largest checkpoint size, among `certs`, that has `k` valid host
    /// cosignatures and whose head is the one this log computes. Entries past it are visible but not yet relied on.
    #[must_use]
    pub fn committed(&self, certs: &[SignedCheckpoint]) -> u64 {
        certs
            .iter()
            .filter(|c| self.head_at(c.cp.size) == Some(c.cp.head))
            .filter(|c| {
                self.hist
                    .iter()
                    .find(|cfg| cfg.epoch == c.cp.epoch)
                    .is_some_and(|cfg| c.count(&self.room, cfg) >= cfg.k)
            })
            .map(|c| c.cp.size)
            .max()
            .unwrap_or(0)
    }

    /// Largest checkpoint size backed by `k` hybrid (Ed25519 + ML-DSA-65) host cosignatures.
    #[must_use]
    pub fn committed_pq(&self, certs: &[SignedCheckpoint]) -> u64 {
        certs
            .iter()
            .filter(|c| self.head_at(c.cp.size) == Some(c.cp.head))
            .filter(|c| {
                self.hist
                    .iter()
                    .find(|cfg| cfg.epoch == c.cp.epoch)
                    .is_some_and(|cfg| c.pq_count(&self.room, cfg) >= cfg.k)
            })
            .map(|c| c.cp.size)
            .max()
            .unwrap_or(0)
    }

    /// The configuration in force at a log of `size` entries: the last epoch whose entry lies inside it.
    #[must_use]
    pub fn config_at(&self, size: u64) -> &Config {
        let n = self.starts.iter().take_while(|s| **s < size.max(1)).count();
        &self.hist[n.max(1) - 1]
    }

    /// The hosts and threshold as of the committed point: what a guest should rely on, since entries past it
    /// might still be replaced.
    #[must_use]
    pub fn committed_config(&self, certs: &[SignedCheckpoint]) -> &Config {
        self.config_at(self.committed(certs))
    }

    /// Entries from `from` on, at most `max`, for a follower asking to catch up.
    #[must_use]
    pub fn since(&self, from: u64, max: usize) -> Vec<Entry> {
        self.entries
            .iter()
            .skip(usize::try_from(from).unwrap_or(usize::MAX))
            .take(max)
            .cloned()
            .collect()
    }
}

/// What a follower holds before it has the genesis.
#[derive(Debug, Clone)]
pub struct FollowerLog {
    room: Key,
    log: Option<RoomLog>,
}

impl FollowerLog {
    #[must_use]
    pub fn log(&self) -> Option<&RoomLog> {
        self.log.as_ref()
    }

    #[must_use]
    pub fn log_mut(&mut self) -> Option<&mut RoomLog> {
        self.log.as_mut()
    }

    /// Accept entries in order. The first must be the genesis, signed by the room's root key; every later one is
    /// checked against the leader it names. Stops at the first bad entry and reports it, keeping what was good.
    ///
    /// # Errors
    ///
    /// The first entry that does not check.
    pub fn accept_all(&mut self, entries: Vec<Entry>) -> Result<usize, LogError> {
        let mut n = 0;
        for e in entries {
            match self.log.as_mut() {
                None => {
                    self.log = Some(verify_genesis(&self.room, &e)?);
                }
                Some(l) => {
                    if !l.take(e)? {
                        continue;
                    }
                }
            }
            n += 1;
        }
        Ok(n)
    }

    #[must_use]
    pub fn into_log(self) -> Option<RoomLog> {
        self.log
    }
}

fn verify_genesis(room: &Key, g: &Entry) -> Result<RoomLog, LogError> {
    if g.index != 0
        || g.prev != [0; 32]
        || g.kind != "epoch"
        || g.signer != *room
        || g.payload.len() > MAX_PAYLOAD
    {
        return Err(LogError::BadGenesis(g.index));
    }
    g.check_signature(room)?;
    let cfg = Config::from_payload(0, &g.payload)?;
    if g.epoch != cfg.epoch || cfg.epoch != 0 {
        return Err(LogError::BadGenesis(g.index));
    }
    Ok(RoomLog {
        room: *room,
        entries: vec![g.clone()],
        hist: vec![cfg.clone()],
        starts: vec![0],
        cfg,
    })
}

/// The host signatures an epoch entry carries.
fn epoch_sigs(e: &Entry) -> Result<BTreeMap<Key, [u8; 64]>, LogError> {
    let bad = || LogError::Malformed(e.index, "epoch (signatures)".into());
    let v: Value = serde_json::from_slice(&e.payload).map_err(|_| bad())?;
    let mut out = BTreeMap::new();
    for (k, s) in v["sigs"].as_object().ok_or_else(bad)? {
        out.insert(
            unhex(k).ok_or_else(bad)?,
            unhex(s.as_str().ok_or_else(bad)?).ok_or_else(bad)?,
        );
    }
    Ok(out)
}

fn validate_event(e: &Entry) -> Result<(), LogError> {
    let bad = |m: &str| LogError::Malformed(e.index, m.to_string());
    match e.kind.as_str() {
        "member.join" | "member.leave" | "member.revoke" => {
            let v: Value =
                serde_json::from_slice(&e.payload).map_err(|_| bad("payload is not json"))?;
            let m = v["member"].as_str().ok_or_else(|| bad("missing member"))?;
            if m.is_empty() || m.len() > 128 || !m.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(bad("member id"));
            }
            Ok(())
        }
        "problem.register" => promotion::parse_register(e).map(|_| ()),
        "promotion" => promotion::parse_promotion(e).map(|_| ()),
        other => Err(bad(&format!("kind {other}"))),
    }
}

#[must_use]
pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

#[must_use]
pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The log as text, one JSON object per line, for a host's state directory.
pub mod text {
    use super::{hex, unhex, Entry, FollowerLog, Key, LogError, RoomLog};
    use serde_json::{json, Value};

    pub fn unhex_vec(s: &str) -> Option<Vec<u8>> {
        if !s.len().is_multiple_of(2) || !s.is_ascii() {
            return None;
        }
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
            .collect()
    }

    #[must_use]
    pub fn to_text(l: &RoomLog) -> String {
        let mut out = String::new();
        for e in l.entries() {
            out.push_str(&entry_to_json(e).to_string());
            out.push('\n');
        }
        out
    }

    /// Parse and re-verify; a stored log is believed no more than one heard from a peer.
    ///
    /// # Errors
    ///
    /// Fails if a line does not parse or an entry does not check.
    pub fn from_text(room: Key, s: &str) -> Result<Option<RoomLog>, LogError> {
        let mut entries = Vec::new();
        for (i, line) in s.lines().filter(|l| !l.trim().is_empty()).enumerate() {
            let v: Value = serde_json::from_str(line)
                .map_err(|_| LogError::Malformed(i as u64, "line".into()))?;
            entries.push(
                entry_from_json(&v).ok_or_else(|| LogError::Malformed(i as u64, "line".into()))?,
            );
        }
        let mut f = FollowerLog { room, log: None };
        f.accept_all(entries)?;
        Ok(f.into_log())
    }

    pub fn entry_from_json(v: &Value) -> Option<Entry> {
        let payload = if let Some(s) = v.get("payload").and_then(Value::as_str) {
            s.as_bytes().to_vec()
        } else if let Some(h) = v.get("payload_hex").and_then(Value::as_str) {
            unhex_vec(h)?
        } else {
            return None;
        };
        Some(Entry {
            epoch: v["epoch"].as_u64()?,
            index: v["index"].as_u64()?,
            prev: unhex(v["prev"].as_str()?)?,
            kind: v["kind"].as_str()?.to_string(),
            payload,
            time_ms: v["time_ms"].as_u64()?,
            signer: unhex(v["signer"].as_str()?)?,
            sig: unhex(v["sig"].as_str()?)?,
        })
    }

    #[must_use]
    pub fn equivocation_to_json(q: &super::Equivocation) -> Value {
        json!({ "a": entry_to_json(&q.a), "b": entry_to_json(&q.b) })
    }

    pub fn equivocation_from_json(v: &Value) -> Option<super::Equivocation> {
        Some(super::Equivocation {
            a: entry_from_json(&v["a"])?,
            b: entry_from_json(&v["b"])?,
        })
    }

    #[must_use]
    pub fn entry_to_json(e: &Entry) -> Value {
        match std::str::from_utf8(&e.payload) {
            Ok(s) => json!({
                "epoch": e.epoch, "index": e.index, "prev": hex(&e.prev), "kind": e.kind,
                "payload": s, "time_ms": e.time_ms,
                "signer": hex(&e.signer), "sig": hex(&e.sig),
            }),
            Err(_) => json!({
                "epoch": e.epoch, "index": e.index, "prev": hex(&e.prev), "kind": e.kind,
                "payload_hex": hex(&e.payload), "time_ms": e.time_ms,
                "signer": hex(&e.signer), "sig": hex(&e.sig),
            }),
        }
    }
}

#[cfg(test)]
// REQ: ROOM-9, ROOM-10
mod tests {
    use super::*;
    use rand_core::{OsRng, RngCore};

    fn key() -> SigningKey {
        let mut b = [0u8; 32];
        OsRng.fill_bytes(&mut b);
        SigningKey::from_bytes(&b)
    }
    fn pk(k: &SigningKey) -> Key {
        k.verifying_key().to_bytes()
    }

    struct Room {
        root: SigningKey,
        hosts: Vec<SigningKey>,
    }

    fn room(n: usize, k: usize) -> (Room, RoomLog) {
        let root = key();
        let hosts: Vec<SigningKey> = (0..n).map(|_| key()).collect();
        let cfg = Config {
            epoch: 0,
            leader: pk(&hosts[0]),
            hosts: hosts.iter().map(pk).collect(),
            k,
        };
        let log = RoomLog::genesis(&root, cfg, 1).unwrap();
        (Room { root, hosts }, log)
    }

    fn join(log: &mut RoomLog, r: &Room, m: &str) {
        log.append(&r.hosts[0], "member.join", &json!({"member": m}), 10)
            .unwrap();
    }

    #[test]
    fn a_threshold_must_be_more_than_half_and_at_most_all() {
        let root = key();
        let hosts: BTreeSet<Key> = (0..4).map(|_| pk(&key())).collect();
        let leader = *hosts.iter().next().unwrap();
        for k in [0usize, 1, 2, 5] {
            let c = Config {
                epoch: 0,
                leader,
                hosts: hosts.clone(),
                k,
            };
            assert!(RoomLog::genesis(&root, c, 1).is_err(), "k={k}");
        }
        for k in [3usize, 4] {
            let c = Config {
                epoch: 0,
                leader,
                hosts: hosts.clone(),
                k,
            };
            assert!(RoomLog::genesis(&root, c, 1).is_ok(), "k={k}");
        }
        let stranger = Config {
            epoch: 0,
            leader: pk(&key()),
            hosts,
            k: 3,
        };
        assert!(
            RoomLog::genesis(&root, stranger, 1).is_err(),
            "leader must be a host"
        );
    }

    #[test]
    fn a_follower_that_knows_only_the_root_key_rebuilds_the_same_log_and_roster() {
        let (r, mut leader) = room(3, 2);
        join(&mut leader, &r, "aaa");
        join(&mut leader, &r, "bbb");
        leader
            .append(&r.hosts[0], "member.revoke", &json!({"member": "aaa"}), 11)
            .unwrap();
        let mut f = RoomLog::follower(pk(&r.root));
        assert_eq!(f.accept_all(leader.entries().to_vec()).unwrap(), 4);
        let fl = f.log().unwrap();
        assert_eq!(fl.head(), leader.head());
        assert_eq!(fl.roster(), leader.roster());
        assert_eq!(fl.roster().members, BTreeSet::from(["bbb".to_string()]));
        assert_eq!(fl.roster().banned, BTreeSet::from(["aaa".to_string()]));
        assert!(fl.verify().is_ok());
        // Feeding the same entries again changes nothing.
        assert_eq!(f.accept_all(leader.entries().to_vec()).unwrap(), 0);
    }

    #[test]
    fn a_banned_member_cannot_be_rejoined_by_replay() {
        let (r, mut l) = room(1, 1);
        l.append(&r.hosts[0], "member.revoke", &json!({"member": "x"}), 5)
            .unwrap();
        join(&mut l, &r, "x");
        assert!(l.roster().members.is_empty());
    }

    #[test]
    fn only_the_leader_can_append_and_only_the_leader_is_believed() {
        let (r, mut l) = room(3, 2);
        assert_eq!(
            l.append(&r.hosts[1], "member.join", &json!({"member": "a"}), 5)
                .unwrap_err(),
            LogError::WrongKey
        );
        // A follower hand-making an entry in the leader's name fails on the signature; in its own, on the signer.
        let mut f = RoomLog::follower(pk(&r.root));
        f.accept_all(l.entries().to_vec()).unwrap();
        let forged = Entry {
            epoch: 0,
            index: 1,
            prev: l.head(),
            kind: "member.join".into(),
            payload: br#"{"member":"evil"}"#.to_vec(),
            time_ms: 5,
            signer: pk(&r.hosts[1]),
            sig: [0; 64],
        }
        .sign(&l.room, &r.hosts[1]);
        assert_eq!(
            f.log_mut().unwrap().accept(forged).unwrap_err(),
            LogError::NotLeader(1)
        );
        let mut lied = Entry {
            epoch: 0,
            index: 1,
            prev: l.head(),
            kind: "member.join".into(),
            payload: br#"{"member":"evil"}"#.to_vec(),
            time_ms: 5,
            signer: pk(&r.hosts[0]),
            sig: [0; 64],
        }
        .sign(&l.room, &r.hosts[1]);
        assert_eq!(
            f.log_mut().unwrap().accept(lied.clone()).unwrap_err(),
            LogError::BadSignature(1)
        );
        lied.sig = [0; 64];
        assert!(f.log_mut().unwrap().accept(lied).is_err());
    }

    #[test]
    fn tampering_reordering_and_gaps_are_refused() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        join(&mut l, &r, "b");
        let es = l.entries().to_vec();
        // Skip an entry.
        let mut f = RoomLog::follower(pk(&r.root));
        assert!(f.accept_all(vec![es[0].clone(), es[2].clone()]).is_err());
        // Change a payload.
        let mut f = RoomLog::follower(pk(&r.root));
        let mut t = es[1].clone();
        t.payload = br#"{"member":"zzz"}"#.to_vec();
        assert!(f.accept_all(vec![es[0].clone(), t]).is_err());
        // A different entry at an index already held is a conflict, not a duplicate.
        let mut f = RoomLog::follower(pk(&r.root));
        f.accept_all(es.clone()).unwrap();
        let mut other = RoomLog::genesis(&r.root, l.config().clone(), 1).unwrap();
        other
            .append(&r.hosts[0], "member.join", &json!({"member": "q"}), 10)
            .unwrap();
        assert!(f
            .accept_all(vec![es[0].clone(), other.entries()[1].clone()])
            .is_err());
        // Unknown kinds and oversized payloads.
        assert!(l.append(&r.hosts[0], "boot", &json!({}), 12).is_err());
        let big = "a".repeat(MAX_PAYLOAD);
        assert!(l
            .append(&r.hosts[0], "member.join", &json!({"member": big}), 12)
            .is_err());
    }

    #[test]
    fn a_genesis_not_signed_by_the_room_key_is_refused() {
        let (r, l) = room(3, 2);
        let mut impostor = RoomLog::follower(pk(&key()));
        assert!(
            impostor.accept_all(l.entries().to_vec()).is_err(),
            "another room's root key must not accept it"
        );
        let mut forged = l.entries()[0].clone();
        forged.signer = pk(&r.hosts[0]);
        let mut f = RoomLog::follower(pk(&r.root));
        assert!(f.accept_all(vec![forged]).is_err());
    }

    #[test]
    fn nothing_is_committed_until_k_hosts_have_cosigned_a_checkpoint_they_replayed() {
        let (r, mut leader) = room(3, 2);
        join(&mut leader, &r, "a");
        let mut f1 = RoomLog::follower(pk(&r.root));
        f1.accept_all(leader.entries().to_vec()).unwrap();
        let f1 = f1.into_log().unwrap();
        let mut f2 = RoomLog::follower(pk(&r.root));
        f2.accept_all(leader.entries().to_vec()).unwrap();
        let f2 = f2.into_log().unwrap();

        // The leader's own signature is one of k=2: not committed.
        let mut sc = leader
            .sign_checkpoint(&r.hosts[0], leader.checkpoint())
            .unwrap();
        assert_eq!(leader.committed(&[sc.clone()]), 0);
        // One follower's cosignature makes two.
        let (who, sig) = f1.cosign(&r.hosts[1], &sc.cp).unwrap();
        assert!(sc.add(&leader.room, &leader.cfg, who, sig));
        assert_eq!(leader.committed(&[sc.clone()]), 2);
        // The other follower learns the certificate and sees the same commit point.
        assert_eq!(f2.committed(&[sc.clone()]), 2);
        // A duplicate or a stranger's signature adds nothing.
        assert!(!sc.add(&leader.room, &leader.cfg, who, sig));
        let outsider = key();
        let (_, osig) = (
            0,
            outsider.sign(&sc.cp.signing_bytes(&leader.room)).to_bytes(),
        );
        assert!(!sc.add(&leader.room, &leader.cfg, pk(&outsider), osig));
        assert_eq!(sc.count(&leader.room, &leader.cfg), 2);
    }

    #[test]
    fn a_host_refuses_to_cosign_what_it_has_not_replayed_or_that_differs() {
        let (r, mut leader) = room(3, 2);
        join(&mut leader, &r, "a");
        let ahead = leader.checkpoint(); // size 2
        let mut behind = RoomLog::follower(pk(&r.root));
        behind.accept_all(leader.entries()[..1].to_vec()).unwrap();
        let behind = behind.into_log().unwrap();
        assert!(
            behind.cosign(&r.hosts[1], &ahead).is_err(),
            "has not reached that size"
        );
        let forked = Checkpoint {
            head: [9; 32],
            ..ahead.clone()
        };
        assert!(
            leader.cosign(&r.hosts[1], &forked).is_err(),
            "different head"
        );
        assert!(leader.cosign(&key(), &ahead).is_err(), "not a host");
        let other_room = Checkpoint { epoch: 1, ..ahead };
        assert!(leader.cosign(&r.hosts[1], &other_room).is_err());
    }

    #[test]
    fn a_certificate_does_not_carry_over_to_another_room() {
        let (r, mut leader) = room(3, 2);
        join(&mut leader, &r, "a");
        let mut sc = leader
            .sign_checkpoint(&r.hosts[0], leader.checkpoint())
            .unwrap();
        let (w, s) = leader.cosign(&r.hosts[1], &sc.cp).unwrap();
        sc.add(&leader.room, &leader.cfg, w, s);
        // The same hosts and entries under a different root key: signatures were over the first room's id.
        let other_root = key();
        let other = RoomLog::genesis(&other_root, leader.config().clone(), 1).unwrap();
        assert_eq!(sc.count(&other.room, other.config()), 0);
    }

    #[test]
    fn committed_ignores_a_certificate_for_a_head_this_log_does_not_have() {
        let (r, mut a) = room(3, 2);
        join(&mut a, &r, "x");
        let mut b = RoomLog::genesis(&r.root, a.config().clone(), 1).unwrap();
        b.append(&r.hosts[0], "member.join", &json!({"member": "y"}), 10)
            .unwrap();
        let mut sc = b.sign_checkpoint(&r.hosts[0], b.checkpoint()).unwrap();
        let (w, s) = b.cosign(&r.hosts[1], &sc.cp).unwrap();
        sc.add(&b.room, &b.cfg, w, s);
        assert_eq!(b.committed(&[sc.clone()]), 2);
        assert_eq!(
            a.committed(&[sc]),
            0,
            "a forked history must not look committed"
        );
    }

    #[test]
    fn text_round_trips_and_a_tampered_file_is_refused() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let t = text::to_text(&l);
        let back = text::from_text(pk(&r.root), &t).unwrap().unwrap();
        assert_eq!(back.head(), l.head());
        let evil = t.replace("member.join", "member.leave");
        assert!(text::from_text(pk(&r.root), &evil).is_err());
        assert!(text::from_text(pk(&key()), &t).is_err(), "wrong room key");
        assert!(text::from_text(pk(&r.root), "").unwrap().is_none());
    }

    #[test]
    fn three_hosts_converge_on_head_roster_and_commit_point() {
        // The whole M1 loop with no network: leader appends, followers pull and verify, cosignatures flow back.
        let (r, mut leader) = room(3, 2);
        let mut f1 = RoomLog::follower(pk(&r.root));
        let mut f2 = RoomLog::follower(pk(&r.root));
        let mut certs: Vec<SignedCheckpoint> = Vec::new();
        for m in ["a", "b", "c"] {
            join(&mut leader, &r, m);
            let mut sc = leader
                .sign_checkpoint(&r.hosts[0], leader.checkpoint())
                .unwrap();
            for (f, host) in [(&mut f1, &r.hosts[1]), (&mut f2, &r.hosts[2])] {
                let have = f.log().map_or(0, RoomLog::len);
                f.accept_all(leader.since(have, 1000)).unwrap();
                let (w, s) = f.log().unwrap().cosign(host, &sc.cp).unwrap();
                sc.add(&leader.room, &leader.cfg, w, s);
            }
            certs.push(sc);
        }
        let (l1, l2) = (f1.log().unwrap(), f2.log().unwrap());
        assert_eq!(l1.head(), leader.head());
        assert_eq!(l2.head(), leader.head());
        assert_eq!(l1.roster(), leader.roster());
        assert_eq!(l2.roster(), leader.roster());
        assert_eq!(leader.committed(&certs), leader.len());
        assert_eq!(l1.committed(&certs), leader.len());
        assert_eq!(l2.committed(&certs), leader.len());
    }

    // ---- M2: failover and equivocation ----

    fn copy(l: &RoomLog) -> RoomLog {
        let mut f = RoomLog::follower(l.room());
        f.accept_all(l.entries().to_vec()).unwrap();
        f.into_log().unwrap()
    }

    /// `who` bids to lead; each of `signers` (index, its own copy of the log) signs if it will.
    fn bid(
        r: &Room,
        who: usize,
        theirs: &RoomLog,
        signers: &[(usize, &RoomLog)],
    ) -> (Proposal, BTreeMap<Key, [u8; 64]>) {
        let p = theirs.proposal(pk(&r.hosts[who]));
        let mut sigs = BTreeMap::new();
        for (i, mine) in signers {
            if let Ok((k, s)) = mine.cosign_proposal(&r.hosts[*i], theirs, &p, None, None) {
                sigs.insert(k, s);
            }
        }
        (p, sigs)
    }

    // REQ: ROOM-12
    #[test]
    fn a_quorum_makes_a_new_leader_and_the_old_one_is_shut_out() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let (mut f1, f2) = (copy(&l), copy(&l));
        let (p, sigs) = bid(&r, 1, &f1, &[(1, &copy(&l)), (2, &f2)]);
        assert_eq!(sigs.len(), 2);
        f1.new_epoch(&r.hosts[1], &p, &sigs, 20).unwrap();
        assert_eq!(
            (f1.config().epoch, f1.config().leader),
            (1, pk(&r.hosts[1]))
        );
        f1.append(&r.hosts[1], "member.join", &json!({"member": "b"}), 30)
            .unwrap();
        f1.verify()
            .expect("the whole log replays from the genesis, across the epoch change");

        // A follower that was in epoch 0 takes the new epoch and the entries after it.
        let mut f2 = f2;
        f2.accept_all(f1.since(2, 10)).unwrap();
        assert_eq!((f2.head(), f2.config().epoch), (f1.head(), 1));

        // The old leader still thinks it leads, but nobody believes what it writes in epoch 0 any more.
        join(&mut l, &r, "stale");
        assert!(f1.accept(l.entries().last().unwrap().clone()).is_err());
        assert!(f2.accept(l.entries().last().unwrap().clone()).is_err());
        // And once it hears of the new epoch it cannot append either.
        let mut old = copy(&f1);
        assert!(old
            .append(&r.hosts[0], "member.join", &json!({"member": "c"}), 40)
            .is_err());
    }

    // REQ: ROOM-12
    #[test]
    fn an_epoch_change_needs_k_host_signatures_that_verify() {
        let (r, mut l) = room(5, 3);
        join(&mut l, &r, "a");
        let copies: Vec<RoomLog> = (0..5).map(|_| copy(&l)).collect();
        let theirs = copy(&l);
        let (p, mut sigs) = bid(&r, 1, &theirs, &[(1, &copies[1]), (2, &copies[2])]);
        let mut t = copy(&l);
        assert_eq!(
            t.new_epoch(&r.hosts[1], &p, &sigs, 20).unwrap_err(),
            LogError::NoQuorum(2)
        );
        // A signature from a key that is not a host, or a host's signature over another bid, adds nothing.
        let stranger = key();
        sigs.insert(
            pk(&stranger),
            stranger.sign(&p.signing_bytes(&l.room())).to_bytes(),
        );
        let other = Proposal {
            leader: pk(&r.hosts[2]),
            ..p.clone()
        };
        sigs.insert(
            pk(&r.hosts[3]),
            r.hosts[3].sign(&other.signing_bytes(&l.room())).to_bytes(),
        );
        assert_eq!(
            t.new_epoch(&r.hosts[1], &p, &sigs, 20).unwrap_err(),
            LogError::NoQuorum(2)
        );
        // The third real signature makes it.
        let (k, s) = copies[3]
            .cosign_proposal(&r.hosts[3], &theirs, &p, None, None)
            .unwrap();
        sigs.insert(k, s);
        t.new_epoch(&r.hosts[1], &p, &sigs, 20).unwrap();
        // Someone else cannot use these signatures to lead.
        let mut u = copy(&l);
        assert!(u.new_epoch(&r.hosts[2], &p, &sigs, 20).is_err());
    }

    // REQ: ROOM-12
    #[test]
    fn a_follower_drops_the_old_leaders_uncommitted_tail_when_the_epoch_entry_comes() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let short = copy(&l);
        join(&mut l, &r, "tail"); // only the old leader and f2 ever saw this
        let mut f2 = copy(&l);
        let (p, sigs) = bid(&r, 1, &short, &[(1, &copy(&short)), (2, &f2)]);
        let mut new = copy(&short);
        new.new_epoch(&r.hosts[1], &p, &sigs, 20).unwrap();
        assert!(f2.roster().members.contains("tail"));
        f2.accept_all(new.since(2, 10)).unwrap();
        assert_eq!(f2.head(), new.head());
        assert!(!f2.roster().members.contains("tail"));
        assert_eq!(f2.len(), 3);
    }

    // REQ: ROOM-12
    #[test]
    fn a_host_will_not_sign_a_bid_whose_log_lacks_what_it_cosigned() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let short = copy(&l);
        join(&mut l, &r, "committed");
        let f2 = copy(&l);
        let last = f2.checkpoint();
        let p = short.proposal(pk(&r.hosts[1]));
        let err = f2
            .cosign_proposal(&r.hosts[2], &short, &p, Some(&last), None)
            .unwrap_err();
        assert!(matches!(err, LogError::BadProposal(m) if m.contains("lacks a checkpoint")));
        // With a log that has it, it signs.
        let p = f2.proposal(pk(&r.hosts[1]));
        assert!(f2
            .cosign_proposal(&r.hosts[2], &f2, &p, Some(&last), None)
            .is_ok());
    }

    // REQ: ROOM-12
    #[test]
    fn a_checkpoint_cosigned_in_the_old_epoch_still_counts_in_the_new_one() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let mut sc = l.sign_checkpoint(&r.hosts[0], l.checkpoint()).unwrap();
        let f2 = copy(&l);
        let (who, sig) = f2.cosign(&r.hosts[2], &sc.cp).unwrap();
        assert!(sc.add(&l.room(), l.config(), who, sig));
        assert_eq!(l.committed(std::slice::from_ref(&sc)), 2);
        let mut f1 = copy(&l);
        let (p, sigs) = bid(&r, 1, &f1, &[(1, &copy(&l)), (2, &f2)]);
        f1.new_epoch(&r.hosts[1], &p, &sigs, 20).unwrap();
        assert_eq!(f1.committed(std::slice::from_ref(&sc)), 2);
        // But a checkpoint cannot be cosigned in the old epoch any more once the log is in the new one.
        assert!(f1.cosign(&r.hosts[2], &sc.cp).is_err());
    }

    // REQ: ROOM-12
    #[test]
    fn two_bids_for_one_epoch_never_both_reach_k_over_1000_schedules() {
        let (r, mut l) = room(5, 3);
        join(&mut l, &r, "a");
        let logs: Vec<RoomLog> = (0..5).map(|_| copy(&l)).collect();
        let bids = [
            logs[1].proposal(pk(&r.hosts[1])),
            logs[2].proposal(pk(&r.hosts[2])),
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut won1, mut won2, mut neither) = (0, 0, 0);
        for _ in 0..1000 {
            let mut promised: Vec<Option<Proposal>> = vec![None; 5];
            let mut got: [BTreeMap<Key, [u8; 64]>; 2] = Default::default();
            // Each host hears the bids in some order and may miss one or both.
            for h in 0..5 {
                let order = if next() % 2 == 0 { [0, 1] } else { [1, 0] };
                for b in order {
                    if next() % 4 == 0 {
                        continue; // message lost
                    }
                    if let Ok((k, s)) = logs[h].cosign_proposal(
                        &r.hosts[h],
                        &logs[b + 1],
                        &bids[b],
                        None,
                        promised[h].as_ref(),
                    ) {
                        promised[h] = Some(bids[b].clone());
                        got[b].insert(k, s);
                    }
                }
            }
            let (a, b) = (got[0].len() >= 3, got[1].len() >= 3);
            assert!(!(a && b), "two leaders in one epoch");
            match (a, b) {
                (true, _) => won1 += 1,
                (_, true) => won2 += 1,
                _ => neither += 1,
            }
        }
        assert!(
            won1 > 0 && won2 > 0 && neither > 0,
            "{won1}/{won2}/{neither}: the schedules should vary"
        );
    }

    // REQ: ROOM-13
    #[test]
    fn two_entries_at_one_index_from_one_key_are_a_proof_anyone_can_check() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let a = l.entries()[1].clone();
        let b = Entry {
            payload: json!({"member": "other"}).to_string().into_bytes(),
            ..a.clone()
        }
        .sign(&l.room(), &r.hosts[0]);
        let q = Equivocation {
            a: a.clone(),
            b: b.clone(),
        };
        assert_eq!(q.verify(&l.room()).unwrap(), pk(&r.hosts[0]));
        // Another room's key does not accept it: the signatures cover the room.
        assert!(q.verify(&pk(&key())).is_err());
        // Not a proof: one entry twice, different positions, different keys, a bad signature.
        assert!(Equivocation {
            a: a.clone(),
            b: a.clone()
        }
        .verify(&l.room())
        .is_err());
        let moved = Entry {
            index: 5,
            ..a.clone()
        }
        .sign(&l.room(), &r.hosts[0]);
        assert!(Equivocation {
            a: a.clone(),
            b: moved
        }
        .verify(&l.room())
        .is_err());
        let other_key = Entry { ..b.clone() }.sign(&l.room(), &r.hosts[1]);
        let other_key = Entry {
            signer: pk(&r.hosts[1]),
            ..other_key
        }
        .sign(&l.room(), &r.hosts[1]);
        assert!(Equivocation {
            a: a.clone(),
            b: other_key
        }
        .verify(&l.room())
        .is_err());
        let mut forged = b.clone();
        forged.sig[0] ^= 1;
        assert!(Equivocation { a, b: forged }.verify(&l.room()).is_err());
    }

    #[test]
    fn config_rejects_overflowing_k_without_panicking() {
        let leader = pk(&key());
        let mut hosts = BTreeSet::new();
        hosts.insert(leader);
        let bad = Config {
            epoch: 0,
            leader,
            hosts,
            k: usize::MAX / 2 + 1,
        };
        assert!(matches!(bad.check(), Err(LogError::BadConfig(_))));
        let raw = json!({
            "epoch": 0,
            "leader": hex(&leader),
            "hosts": [hex(&leader)],
            "k": (usize::MAX / 2 + 1) as u64,
        })
        .to_string()
        .into_bytes();
        assert!(Config::from_payload(0, &raw).is_err());
    }

    // REQ: ROOM-13
    #[test]
    fn a_follower_that_is_shown_the_other_entry_gets_the_proof() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let mut other = copy(&l);
        // The leader writes a different entry 2 for another follower.
        join(&mut l, &r, "x");
        other
            .append(&r.hosts[0], "member.join", &json!({"member": "y"}), 11)
            .unwrap();
        let q = l
            .find_equivocation(other.since(2, 1).as_slice())
            .expect("proof");
        assert_eq!(q.verify(&l.room()).unwrap(), pk(&r.hosts[0]));
        // The same entry, or one in another epoch's position, is no proof.
        assert!(l.find_equivocation(l.since(2, 1).as_slice()).is_none());
        // A legitimately replaced entry (new epoch) is not an equivocation by the old leader.
        let (p, sigs) = bid(&r, 1, &copy(&l), &[(1, &copy(&l)), (2, &copy(&l))]);
        let mut new = copy(&l);
        new.new_epoch(&r.hosts[1], &p, &sigs, 40).unwrap();
        assert!(l.find_equivocation(new.since(3, 1).as_slice()).is_none());
    }

    // REQ: ROOM-12
    #[test]
    fn a_bid_that_lost_to_a_rival_is_retried_at_a_higher_epoch() {
        let (r, mut l) = room(5, 3);
        join(&mut l, &r, "a");
        let logs: Vec<RoomLog> = (0..5).map(|_| copy(&l)).collect();
        let first = logs[1].proposal(pk(&r.hosts[1]));
        // Hosts 3 and 4 signed host 1's bid but it never got a third signature (messages lost).
        let promised = Some(first.clone());
        let bid1 = logs[2].proposal(pk(&r.hosts[2]));
        for h in [3, 4] {
            assert!(
                logs[h]
                    .cosign_proposal(&r.hosts[h], &logs[2], &bid1, None, promised.as_ref())
                    .is_err(),
                "a rival for the same epoch is refused"
            );
        }
        // Host 2 bids again one epoch higher; the same hosts sign it, and the epoch number skips.
        let bid2 = logs[2].proposal_at(pk(&r.hosts[2]), bid1.epoch + 1);
        let mut sigs = BTreeMap::new();
        for h in [2, 3, 4] {
            let (k, s) = logs[h]
                .cosign_proposal(&r.hosts[h], &logs[2], &bid2, None, promised.as_ref())
                .unwrap();
            sigs.insert(k, s);
        }
        let mut new = copy(&logs[2]);
        new.new_epoch(&r.hosts[2], &bid2, &sigs, 50).unwrap();
        assert_eq!(new.config().epoch, 2);
        new.verify().unwrap();
        let mut follower = copy(&l);
        follower.accept_all(new.since(2, 5)).unwrap();
        assert_eq!(follower.head(), new.head());
    }

    // ---- M3: host-set change and key rotation ----

    /// Leader `0` asks to change the host set to `hosts`; each of `signers` signs if it will.
    fn reconfig_bid(
        r: &Room,
        leader: &RoomLog,
        hosts: &[Key],
        k: usize,
        signers: &[(&SigningKey, &RoomLog)],
    ) -> (Proposal, BTreeMap<Key, [u8; 64]>) {
        let p = leader.proposal_for(
            leader.config().leader,
            leader.config().epoch + 1,
            hosts.iter().copied().collect(),
            k,
        );
        let mut sigs = BTreeMap::new();
        for (key, mine) in signers {
            if let Ok((who, s)) = mine.cosign_proposal(key, leader, &p, None, None) {
                sigs.insert(who, s);
            }
        }
        let _ = r;
        (p, sigs)
    }

    // REQ: ROOM-14
    #[test]
    fn rotating_a_host_key_needs_a_quorum_of_the_old_set_and_shuts_the_old_key_out() {
        let (r, mut l) = room(5, 3);
        join(&mut l, &r, "a");
        let old_cp = {
            let mut sc = l.sign_checkpoint(&r.hosts[0], l.checkpoint()).unwrap();
            for i in [1, 4] {
                let (who, sig) = copy(&l).cosign(&r.hosts[i], &sc.cp).unwrap();
                assert!(sc.add(&l.room(), l.config(), who, sig));
            }
            sc
        };
        assert_eq!(l.committed(std::slice::from_ref(&old_cp)), 2);

        // Host 4's key is replaced by a new one.
        let fresh = key();
        let mut hosts: Vec<Key> = r.hosts[..4].iter().map(pk).collect();
        hosts.push(pk(&fresh));
        let copies: Vec<RoomLog> = (0..5).map(|_| copy(&l)).collect();
        let (p, sigs) = reconfig_bid(
            &r,
            &l,
            &hosts,
            3,
            &[
                (&r.hosts[0], &copies[0]),
                (&r.hosts[1], &copies[1]),
                (&r.hosts[2], &copies[2]),
            ],
        );
        assert_eq!(sigs.len(), 3);
        l.new_epoch(&r.hosts[0], &p, &sigs, 20).unwrap();
        assert_eq!(l.config().epoch, 1);
        assert!(
            l.config().hosts.contains(&pk(&fresh)) && !l.config().hosts.contains(&pk(&r.hosts[4]))
        );
        l.verify()
            .expect("the log replays from the genesis across the change");

        // The removed key can no longer cosign, lead or have a signature counted.
        let cp = l.checkpoint();
        assert_eq!(l.cosign(&r.hosts[4], &cp).unwrap_err(), LogError::WrongKey);
        let mut sc = l.sign_checkpoint(&r.hosts[0], cp.clone()).unwrap();
        let sig = r.hosts[4].sign(&cp.signing_bytes(&l.room())).to_bytes();
        assert!(!sc.add(&l.room(), l.config(), pk(&r.hosts[4]), sig));
        let stale = Entry {
            epoch: 1,
            index: l.len(),
            prev: l.head(),
            kind: "member.join".into(),
            payload: json!({"member": "x"}).to_string().into_bytes(),
            time_ms: 30,
            signer: pk(&r.hosts[4]),
            sig: [0; 64],
        }
        .sign(&l.room(), &r.hosts[4]);
        assert_eq!(l.accept(stale).unwrap_err(), LogError::NotLeader(l.len()));

        // Its signature on a checkpoint from before still counts for that epoch, and only that one.
        assert_eq!(l.committed(std::slice::from_ref(&old_cp)), 2);
        // The new key cosigns in the new epoch after replaying the log.
        let newcomer = copy(&l);
        let (who, sig) = newcomer.cosign(&fresh, &cp).unwrap();
        assert!(sc.add(&l.room(), l.config(), who, sig));
        assert_eq!(sc.count(&l.room(), l.config()), 2);
    }

    // REQ: ROOM-14
    #[test]
    fn a_big_change_needs_a_quorum_of_the_new_set_too() {
        let (r, mut l) = room(5, 3);
        join(&mut l, &r, "a");
        let newcomers: Vec<SigningKey> = (0..3).map(|_| key()).collect();
        // Keep hosts 0 and 1, bring in three new ones.
        let mut hosts: Vec<Key> = r.hosts[..2].iter().map(pk).collect();
        hosts.extend(newcomers.iter().map(pk));
        let copies: Vec<RoomLog> = (0..5).map(|_| copy(&l)).collect();
        let old_quorum = [
            (&r.hosts[0], &copies[0]),
            (&r.hosts[1], &copies[1]),
            (&r.hosts[2], &copies[2]),
        ];
        // Three old hosts agree, but only two of them are in the new set: not enough there.
        let (p, sigs) = reconfig_bid(&r, &l, &hosts, 3, &old_quorum);
        assert_eq!(sigs.len(), 3);
        assert_eq!(
            copy(&l).new_epoch(&r.hosts[0], &p, &sigs, 20).unwrap_err(),
            LogError::NoQuorum(2)
        );
        // One newcomer, who replays the log first, makes the new quorum; the old one still holds.
        let mut sigs = sigs;
        let nc = copy(&l);
        let (who, sig) = nc
            .cosign_proposal(&newcomers[0], &l, &p, None, None)
            .unwrap();
        sigs.insert(who, sig);
        copy(&l).new_epoch(&r.hosts[0], &p, &sigs, 20).unwrap();
        // The new set alone is not enough either: the old hosts must agree to the change.
        let mut only_new = BTreeMap::new();
        for (i, n) in newcomers.iter().enumerate() {
            let (who, sig) = copy(&l).cosign_proposal(n, &l, &p, None, None).unwrap();
            only_new.insert(who, sig);
            let _ = i;
        }
        let (who, sig) = copies[0]
            .cosign_proposal(&r.hosts[0], &l, &p, None, None)
            .unwrap();
        only_new.insert(who, sig);
        assert_eq!(
            copy(&l)
                .new_epoch(&r.hosts[0], &p, &only_new, 20)
                .unwrap_err(),
            LogError::NoQuorum(2)
        );
    }

    // REQ: ROOM-14
    #[test]
    fn a_change_that_breaks_the_rules_gets_no_signatures() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let copies: Vec<RoomLog> = (0..3).map(|_| copy(&l)).collect();
        let all: Vec<Key> = r.hosts.iter().map(pk).collect();
        // A threshold of one in three hosts would allow two groups to commit.
        let (p, sigs) = reconfig_bid(&r, &l, &all, 1, &[(&r.hosts[1], &copies[1])]);
        assert!(sigs.is_empty(), "k=1 of 3 is refused: {p:?}");
        // The leader cannot remove itself in the same breath.
        let others: Vec<Key> = all[1..].to_vec();
        let (_, sigs) = reconfig_bid(&r, &l, &others, 2, &[(&r.hosts[1], &copies[1])]);
        assert!(sigs.is_empty(), "the leader must be in the new set");
        // Nor can a host that is in neither set be made to sign.
        let outsider = key();
        let (_, sigs) = reconfig_bid(&r, &l, &all, 2, &[(&outsider, &copies[1])]);
        assert!(sigs.is_empty());
        // Removing a host down to two of two is fine: k = 2.
        let two: Vec<Key> = all[..2].to_vec();
        let (p, mut sigs) = reconfig_bid(
            &r,
            &l,
            &two,
            2,
            &[(&r.hosts[0], &copies[0]), (&r.hosts[1], &copies[1])],
        );
        // The old quorum is 2 of 3 and the new is 2 of 2; both are met.
        let mut t = copy(&l);
        t.new_epoch(&r.hosts[0], &p, &sigs, 20).unwrap();
        assert_eq!(t.config().hosts.len(), 2);
        // Host 2, removed, can no longer cosign anything.
        sigs.clear();
        assert!(t.cosign(&r.hosts[2], &t.checkpoint()).is_err());
    }

    // REQ: ROOM-15
    #[test]
    fn a_guest_relies_on_the_host_set_as_of_the_committed_point() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let copies: Vec<RoomLog> = (0..3).map(|_| copy(&l)).collect();
        let newcomer = key();
        let mut hosts: Vec<Key> = r.hosts.iter().map(pk).collect();
        hosts.push(pk(&newcomer));
        let (p, sigs) = reconfig_bid(
            &r,
            &l,
            &hosts,
            3,
            &[
                (&r.hosts[0], &copies[0]),
                (&r.hosts[1], &copies[1]),
                (&r.hosts[2], &copies[2]),
            ],
        );
        let before = l.config().clone();
        l.new_epoch(&r.hosts[0], &p, &sigs, 20).unwrap();
        // Nothing committed yet: the guest still sees the set it can prove (the genesis one), not the proposal.
        assert_eq!(l.committed_config(&[]), &before);
        assert_eq!(l.config_at(l.len() - 1), &before);
        assert_eq!(l.config_at(l.len()).hosts.len(), 4);
        // Once a checkpoint covering the epoch entry has k of the new set's cosignatures, the new set is the one to use.
        let mut sc = l.sign_checkpoint(&r.hosts[0], l.checkpoint()).unwrap();
        for i in [1, 2] {
            let (who, sig) = l.cosign(&r.hosts[i], &sc.cp).unwrap();
            assert!(sc.add(&l.room(), l.config(), who, sig));
        }
        assert_eq!(l.committed_config(std::slice::from_ref(&sc)).hosts.len(), 4);
    }

    // ---- M5: promotion as a quorum rule ----

    use crate::promotion::{Promotion, Receipt, Register, Rule};

    const PROBLEM: Hash = [7; 32];
    const WEIGHTS: Hash = [8; 32];

    fn registered(n: usize, k: usize, j: usize) -> (Room, RoomLog) {
        let (r, mut l) = room(n, k);
        let reg = Register {
            problem: PROBLEM,
            rule: Rule {
                j,
                noise_ppm: 50_000,
                min_gain_ppm: 50_000,
            },
            baseline: 1_000_000,
        };
        l.register_problem(&r.hosts[0], &reg, 5).unwrap();
        (r, l)
    }

    fn receipt(r: &Room, l: &RoomLog, host: usize, cand: Hash, tier: u8, metric: i64) -> Receipt {
        Receipt::sign(
            &l.room(),
            &r.hosts[host],
            &PROBLEM,
            &cand,
            &WEIGHTS,
            [host as u8; 32],
            tier,
            metric,
        )
    }

    fn promo(cand: Hash, receipts: Vec<Receipt>) -> Promotion {
        Promotion {
            problem: PROBLEM,
            candidate: cand,
            weights: WEIGHTS,
            receipts,
        }
    }

    // REQ: ROOM-16
    #[test]
    fn a_rule_must_ask_for_at_least_two_hosts_and_no_more_than_there_are() {
        let (r, mut l) = room(3, 2);
        for (j, ok) in [(1usize, false), (2, true), (4, false)] {
            let reg = Register {
                problem: [j as u8; 32],
                rule: Rule {
                    j,
                    noise_ppm: 1,
                    min_gain_ppm: 1,
                },
                baseline: 1,
            };
            assert_eq!(
                l.register_problem(&r.hosts[0], &reg, 5).is_ok(),
                ok,
                "j={j}"
            );
        }
        // The same problem cannot be registered twice with a different rule.
        let again = Register {
            problem: [2; 32],
            rule: Rule {
                j: 2,
                noise_ppm: 1_000_000,
                min_gain_ppm: 0,
            },
            baseline: 1,
        };
        assert!(l.register_problem(&r.hosts[0], &again, 6).is_err());
    }

    // REQ: ROOM-16
    #[test]
    fn two_distinct_hosts_with_agreeing_receipts_promote_and_the_strength_is_the_lowest_tier() {
        let (r, mut l) = registered(3, 2, 2);
        let cand = [1; 32];
        let p = promo(
            cand,
            vec![
                receipt(&r, &l, 0, cand, 3, 1_200_000),
                receipt(&r, &l, 1, cand, 1, 1_180_000),
            ],
        );
        l.promote(&r.hosts[0], &p, 10).unwrap();
        let f = &l.frontier(None)[&PROBLEM].1;
        assert_eq!(
            (f.candidate, f.metric, f.strength),
            (cand, 1_180_000, 1),
            "lowest metric and lowest tier"
        );
        // A follower replaying from the root key reaches the same frontier.
        let f2 = copy(&l);
        assert_eq!(f2.frontier(None)[&PROBLEM].1, *f);
    }

    // REQ: ROOM-16
    #[test]
    fn one_host_cannot_promote_alone_or_twice_or_with_a_stranger_or_with_edited_numbers() {
        let (r, mut l) = registered(3, 2, 2);
        let cand = [1; 32];
        let good = receipt(&r, &l, 0, cand, 2, 1_200_000);
        // One receipt.
        let e = l
            .promote(&r.hosts[0], &promo(cand, vec![good.clone()]), 10)
            .unwrap_err();
        assert!(
            matches!(&e, LogError::Promotion(_, m) if m.contains("1 distinct")),
            "{e}"
        );
        // The same host twice.
        let e = l
            .promote(
                &r.hosts[0],
                &promo(cand, vec![good.clone(), good.clone()]),
                10,
            )
            .unwrap_err();
        assert!(
            matches!(&e, LogError::Promotion(_, m) if m.contains("once")),
            "{e}"
        );
        // A key that is not a host.
        let stranger = key();
        let s = Receipt::sign(
            &l.room(),
            &stranger,
            &PROBLEM,
            &cand,
            &WEIGHTS,
            [0; 32],
            4,
            1_200_000,
        );
        let e = l
            .promote(&r.hosts[0], &promo(cand, vec![good.clone(), s]), 10)
            .unwrap_err();
        assert!(
            matches!(&e, LogError::Promotion(_, m) if m.contains("not a host")),
            "{e}"
        );
        // A receipt whose number was edited after signing, or that was signed for other weights or another candidate.
        let mut edited = receipt(&r, &l, 1, cand, 2, 1_190_000);
        edited.metric = 2_000_000;
        let e = l
            .promote(&r.hosts[0], &promo(cand, vec![good.clone(), edited]), 10)
            .unwrap_err();
        assert!(
            matches!(&e, LogError::Promotion(_, m) if m.contains("signature")),
            "{e}"
        );
        let other_weights = Receipt::sign(
            &l.room(),
            &r.hosts[1],
            &PROBLEM,
            &cand,
            &[9; 32],
            [1; 32],
            2,
            1_190_000,
        );
        assert!(l
            .promote(
                &r.hosts[0],
                &promo(cand, vec![good.clone(), other_weights]),
                10
            )
            .is_err());
        let other_cand = receipt(&r, &l, 1, [2; 32], 2, 1_190_000);
        assert!(l
            .promote(&r.hosts[0], &promo(cand, vec![good, other_cand]), 10)
            .is_err());
        assert!(
            l.frontier(None)[&PROBLEM].1.candidate == [0; 32],
            "nothing was promoted"
        );
    }

    // REQ: ROOM-16
    #[test]
    fn receipts_must_agree_within_noise_and_the_lowest_must_clear_the_gain() {
        let (r, mut l) = registered(3, 2, 2);
        let cand = [1; 32];
        // 20% apart is more than the 5% noise allowed.
        let e = l
            .promote(
                &r.hosts[0],
                &promo(
                    cand,
                    vec![
                        receipt(&r, &l, 0, cand, 2, 1_500_000),
                        receipt(&r, &l, 1, cand, 2, 1_200_000),
                    ],
                ),
                10,
            )
            .unwrap_err();
        assert!(
            matches!(&e, LogError::Promotion(_, m) if m.contains("differ")),
            "{e}"
        );
        // Both agree, but the lower one is only 3% over the baseline: under the 5% gain.
        let e = l
            .promote(
                &r.hosts[0],
                &promo(
                    cand,
                    vec![
                        receipt(&r, &l, 0, cand, 2, 1_060_000),
                        receipt(&r, &l, 1, cand, 2, 1_030_000),
                    ],
                ),
                10,
            )
            .unwrap_err();
        assert!(
            matches!(&e, LogError::Promotion(_, m) if m.contains("does not beat")),
            "{e}"
        );
        // Promoted, then the same candidate again, and a gain measured against the new frontier.
        l.promote(
            &r.hosts[0],
            &promo(
                cand,
                vec![
                    receipt(&r, &l, 0, cand, 2, 1_300_000),
                    receipt(&r, &l, 1, cand, 2, 1_280_000),
                ],
            ),
            10,
        )
        .unwrap();
        assert!(l
            .promote(
                &r.hosts[0],
                &promo(
                    cand,
                    vec![
                        receipt(&r, &l, 0, cand, 2, 1_300_000),
                        receipt(&r, &l, 1, cand, 2, 1_280_000)
                    ]
                ),
                11
            )
            .is_err());
        let next = [2; 32];
        assert!(
            l.promote(
                &r.hosts[0],
                &promo(
                    next,
                    vec![
                        receipt(&r, &l, 0, next, 2, 1_310_000),
                        receipt(&r, &l, 1, next, 2, 1_300_000)
                    ]
                ),
                12
            )
            .is_err(),
            "1.3 is not 5% over 1.28"
        );
        l.promote(
            &r.hosts[0],
            &promo(
                next,
                vec![
                    receipt(&r, &l, 0, next, 2, 1_400_000),
                    receipt(&r, &l, 1, next, 2, 1_380_000),
                ],
            ),
            13,
        )
        .unwrap();
        assert_eq!(l.frontier(None)[&PROBLEM].1.candidate, next);
    }

    // REQ: ROOM-16
    #[test]
    fn a_leader_that_writes_a_promotion_the_rule_forbids_is_refused_by_every_follower() {
        let (r, l) = registered(3, 2, 2);
        let cand = [1; 32];
        let forged = promotion::promotion_payload(&promo(
            cand,
            vec![receipt(&r, &l, 0, cand, 4, 9_000_000)],
        ));
        // The leader signs it itself, skipping its own checks.
        let e = Entry {
            epoch: 0,
            index: l.len(),
            prev: l.head(),
            kind: "promotion".into(),
            payload: forged.to_string().into_bytes(),
            time_ms: 20,
            signer: pk(&r.hosts[0]),
            sig: [0; 64],
        }
        .sign(&l.room(), &r.hosts[0]);
        let mut f = copy(&l);
        assert!(matches!(f.accept(e).unwrap_err(), LogError::Promotion(..)));
    }

    // REQ: ROOM-16
    #[test]
    fn a_guest_relies_only_on_committed_promotions() {
        let (r, mut l) = registered(3, 2, 2);
        let commit = |l: &RoomLog| {
            let mut sc = l.sign_checkpoint(&r.hosts[0], l.checkpoint()).unwrap();
            let (who, sig) = copy(l).cosign(&r.hosts[1], &sc.cp).unwrap();
            assert!(sc.add(&l.room(), l.config(), who, sig));
            sc
        };
        let reg = commit(&l);
        let cand = [1; 32];
        l.promote(
            &r.hosts[0],
            &promo(
                cand,
                vec![
                    receipt(&r, &l, 0, cand, 2, 1_200_000),
                    receipt(&r, &l, 1, cand, 2, 1_190_000),
                ],
            ),
            10,
        )
        .unwrap();
        assert_eq!(
            l.frontier(Some(std::slice::from_ref(&reg)))[&PROBLEM]
                .1
                .candidate,
            [0; 32],
            "promotion not committed yet"
        );
        let both = commit(&l);
        assert_eq!(l.frontier(Some(&[reg, both]))[&PROBLEM].1.candidate, cand);
    }

    // REQ: ROOM-16
    #[test]
    fn a_removed_host_cannot_help_promote_in_a_later_epoch() {
        let (r, mut l) = registered(3, 2, 2);
        let copies: Vec<RoomLog> = (0..3).map(|_| copy(&l)).collect();
        let hosts: Vec<Key> = r.hosts[..2].iter().map(pk).collect();
        let (p, sigs) = reconfig_bid(
            &r,
            &l,
            &hosts,
            2,
            &[(&r.hosts[0], &copies[0]), (&r.hosts[1], &copies[1])],
        );
        l.new_epoch(&r.hosts[0], &p, &sigs, 20).unwrap();
        let cand = [1; 32];
        let e = l
            .promote(
                &r.hosts[0],
                &promo(
                    cand,
                    vec![
                        receipt(&r, &l, 0, cand, 2, 1_200_000),
                        receipt(&r, &l, 2, cand, 2, 1_190_000),
                    ],
                ),
                30,
            )
            .unwrap_err();
        assert!(
            matches!(&e, LogError::Promotion(_, m) if m.contains("not a host")),
            "{e}"
        );
        l.promote(
            &r.hosts[0],
            &promo(
                cand,
                vec![
                    receipt(&r, &l, 0, cand, 2, 1_200_000),
                    receipt(&r, &l, 1, cand, 2, 1_190_000),
                ],
            ),
            30,
        )
        .unwrap();
    }

    #[test]
    fn non_utf8_entry_payloads_and_equivocations_round_trip_through_json() {
        let (r, l) = room(1, 1);
        let a = Entry {
            epoch: 0,
            index: 1,
            prev: l.head(),
            kind: "note".into(),
            payload: vec![0xff, 0xfe, 0x00, 0x80],
            time_ms: 10,
            signer: pk(&r.hosts[0]),
            sig: [0; 64],
        }
        .sign(&l.room(), &r.hosts[0]);
        let b = Entry {
            payload: vec![0xff, 0xfd, 0x00, 0x80],
            ..a.clone()
        }
        .sign(&l.room(), &r.hosts[0]);

        let ja = text::entry_to_json(&a);
        assert!(ja.get("payload").is_none());
        assert_eq!(ja["payload_hex"], "fffe0080");
        assert_eq!(text::entry_from_json(&ja).unwrap(), a);

        let q = Equivocation {
            a: a.clone(),
            b: b.clone(),
        };
        assert_eq!(q.verify(&l.room()).unwrap(), pk(&r.hosts[0]));
        let round = text::equivocation_from_json(&text::equivocation_to_json(&q)).unwrap();
        assert_eq!(round, q);
        assert_eq!(round.verify(&l.room()).unwrap(), pk(&r.hosts[0]));

        // Identical signing_bytes with a different signature byte is rejected as not an equivocation
        // (and bad signature if invalid, or "same entry twice" when signing_bytes match).
        let mut alt_sig = a.clone();
        alt_sig.sig[0] ^= 1;
        assert!(matches!(
            Equivocation { a, b: alt_sig }.verify(&l.room()),
            Err(LogError::NotEquivocation(_))
        ));
    }

    #[test]
    fn checkpoint_hybrid_ed25519_and_mldsa65_dual_signing_verifies_and_rejects_tamper() {
        let (r, mut l) = room(3, 2);
        join(&mut l, &r, "a");
        let mut sc = l.sign_checkpoint(&r.hosts[0], l.checkpoint()).unwrap();
        assert_eq!(sc.count(&l.room(), l.config()), 1);
        assert_eq!(sc.pq_count(&l.room(), l.config()), 1);
        assert_eq!(l.committed_pq(std::slice::from_ref(&sc)), 0);

        let f1 = copy(&l);
        let (who, sig, pq) = f1.cosign_hybrid(&r.hosts[1], &sc.cp).unwrap();
        assert!(sc.add(&l.room(), l.config(), who, sig));
        let mut bad_pq = pq.clone();
        bad_pq.sig[0] ^= 1;
        assert!(!sc.add_pq(&l.room(), l.config(), who, bad_pq));
        assert_eq!(l.committed_pq(std::slice::from_ref(&sc)), 0);

        // Wrong signer binding (Ed25519 key 2 with host 1's ML-DSA-65 sig) is rejected.
        assert!(!sc.add_pq(&l.room(), l.config(), pk(&r.hosts[2]), pq.clone()));

        assert!(sc.add_pq(&l.room(), l.config(), who, pq));
        assert_eq!(sc.pq_count(&l.room(), l.config()), 2);
        assert_eq!(l.committed_pq(std::slice::from_ref(&sc)), 2);
    }
}
