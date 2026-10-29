//! Shared memory for a room, where every entry says who wrote it, why, and what it came from.
//!
//! Git shares files. Agents also learn things (facts about the codebase, decisions, skills) and want the
//! next agent to have them. That is the part most exposed to poisoning: one false "fact" from a guest is
//! later read by every agent as if it were true. The store applies the defences the memory-security
//! literature converges on, as rules rather than hopes:
//!
//! - **Signed, chained entries.** Each entry is signed by its author and names the entry before it, so
//!   nothing can be dropped, reordered or forged after the fact.
//! - **Write-time admission.** A guest's entry starts *quarantined* and is never recalled until a pinned
//!   reviewer accepts it. The owner's own entries are accepted on write. A `fact` or `decision` with no
//!   evidence is refused outright; evidence is the hash of a history entry or commit that supports it.
//! - **Derivation closure.** An entry lists what it was `derived_from`. If an ancestor is rejected,
//!   retracted, or its author is revoked, everything derived from it is distrusted too, transitively.
//!   This is how "forgetting" a bad contributor actually removes their influence, including laundered
//!   summaries of it.
//! - **Conflicts are surfaced, not merged.** Two trusted entries on the same `topic` with different
//!   bodies are reported by [`MemoryStore::conflicts`] for a person to resolve; `supersedes` lets an
//!   author replace only their own entries.
//! - **Recall is labelled.** [`MemoryStore::recall`] registers what it returns with the
//!   [`TaintTracker`](crate::taint::TaintTracker), at `Reviewed` trust for accepted guest text and
//!   `Trusted` for the owner's, so memory text that an agent later pastes into a command is caught by
//!   the admission broker.
//!
//! All state is derived from an append-only event log ([`MemoryStore::log`]); [`MemoryStore::replay`]
//! rebuilds it and reaches the same decisions.
//!
//! # What this does and does not prove
//! It controls *who may write* and *what stays trusted*. It does not check that a fact is true: a
//! reviewer who accepts a lie has accepted it, and evidence hashes are only as meaningful as the history
//! they point at (use [`MemoryStore::evidence_in`] to check they exist in a log you hold).

use crate::{
    history::Log,
    record::{self, hex},
    taint::{Source, TaintTracker, Trust},
};
use ed25519_dalek::SigningKey;
use std::collections::{BTreeMap, BTreeSet};

const ENTRY: &str = "playbox.memory.v1";
const REVIEW: &str = "playbox.memory.review.v1";
const RETRACT: &str = "playbox.memory.retract.v1";
const REVOKE: &str = "playbox.memory.revoke.v1";

pub const MAX_BODY: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Fact,
    Decision,
    Note,
    Skill,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Fact => "fact",
            Kind::Decision => "decision",
            Kind::Note => "note",
            Kind::Skill => "skill",
        }
    }

    fn parse(s: &str) -> Result<Kind, String> {
        Ok(match s {
            "fact" => Kind::Fact,
            "decision" => Kind::Decision,
            "note" => Kind::Note,
            "skill" => Kind::Skill,
            o => return Err(format!("unknown memory kind {o:?}")),
        })
    }

    fn needs_evidence(self) -> bool {
        matches!(self, Kind::Fact | Kind::Decision)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Quarantined,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub author: [u8; 32],
    pub kind: Kind,
    pub topic: String,
    pub body: String,
    /// Hashes of the history entries or commits that support this.
    pub evidence: Vec<String>,
    pub derived_from: Vec<String>,
    pub supersedes: Option<String>,
    pub time_ms: u64,
    /// The entry before this one in the store (empty for the first).
    pub prev: String,
}

pub struct NewEntry<'a> {
    pub kind: Kind,
    pub topic: &'a str,
    pub body: &'a str,
    pub evidence: &'a [&'a str],
    pub derived_from: &'a [&'a str],
    pub supersedes: Option<&'a str>,
    pub time_ms: u64,
}

impl Entry {
    /// Sign a new entry. `prev` is [`MemoryStore::head`] of the store it will be appended to.
    pub fn sign(new: NewEntry, prev: &str, key: &SigningKey) -> Result<String, String> {
        record::sign(
            ENTRY,
            &[
                ("kind", new.kind.as_str().into()),
                ("topic", new.topic.into()),
                ("body", hex(new.body.as_bytes())),
                ("evidence", new.evidence.join(",")),
                ("derived", new.derived_from.join(",")),
                ("supersedes", new.supersedes.unwrap_or_default().into()),
                ("time", new.time_ms.to_string()),
                ("prev", prev.into()),
            ],
            key,
        )
    }

    fn open(text: &str) -> Result<Entry, String> {
        let r = record::open(ENTRY, text)?;
        Ok(Entry {
            id: hex(&record::hash_parts("playbox.memory.id", &[text.as_bytes()])),
            author: r.signer,
            kind: Kind::parse(r.req("kind")?)?,
            topic: r.req("topic")?.to_string(),
            body: String::from_utf8(record::unhex_vec(r.req("body")?)?).map_err(|_| "body is not utf-8")?,
            evidence: r.list("evidence")?.into_iter().map(String::from).collect(),
            derived_from: r.list("derived")?.into_iter().map(String::from).collect(),
            supersedes: Some(r.req("supersedes")?).filter(|s| !s.is_empty()).map(String::from),
            time_ms: r.num("time")?,
            prev: r.req("prev")?.to_string(),
        })
    }
}

pub struct MemoryStore {
    /// Authors whose entries are accepted on write (the room owner, their own tooling).
    owners: BTreeSet<[u8; 32]>,
    /// Keys allowed to accept, reject, retract and revoke. Includes the owners.
    reviewers: BTreeSet<[u8; 32]>,
    entries: Vec<Entry>,
    by_id: BTreeMap<String, usize>,
    status: BTreeMap<String, Status>,
    retracted: BTreeSet<String>,
    revoked: BTreeSet<[u8; 32]>,
    log: Vec<String>,
}

impl MemoryStore {
    pub fn new(owners: &[[u8; 32]], extra_reviewers: &[[u8; 32]]) -> Self {
        let owners: BTreeSet<_> = owners.iter().copied().collect();
        let reviewers = owners.iter().chain(extra_reviewers).copied().collect();
        MemoryStore {
            owners,
            reviewers,
            entries: Vec::new(),
            by_id: BTreeMap::new(),
            status: BTreeMap::new(),
            retracted: BTreeSet::new(),
            revoked: BTreeSet::new(),
            log: Vec::new(),
        }
    }

    /// Id of the last entry, to put in the next entry's `prev`.
    pub fn head(&self) -> String {
        self.entries.last().map(|e| e.id.clone()).unwrap_or_default()
    }

    pub fn log(&self) -> &[String] {
        &self.log
    }

    /// Rebuild a store from an event log, applying every event under the same rules.
    pub fn replay(owners: &[[u8; 32]], reviewers: &[[u8; 32]], log: &[String]) -> Result<Self, String> {
        let mut s = Self::new(owners, reviewers);
        for (i, t) in log.iter().enumerate() {
            s.apply(t).map_err(|e| format!("event {i}: {e}"))?;
        }
        Ok(s)
    }

    /// Apply one signed event (an entry, a review, a retraction or a revocation).
    pub fn apply(&mut self, text: &str) -> Result<String, String> {
        let domain = text.lines().next().unwrap_or("");
        let id = match domain {
            ENTRY => self.append(text)?,
            REVIEW => self.apply_review(text)?,
            RETRACT => self.apply_retract(text)?,
            REVOKE => self.apply_revoke(text)?,
            _ => return Err("unknown event".into()),
        };
        self.log.push(text.to_string());
        Ok(id)
    }

    fn append(&mut self, text: &str) -> Result<String, String> {
        let e = Entry::open(text)?;
        if e.prev != self.head() {
            return Err("entry does not extend the store: prev is not the head".into());
        }
        if self.revoked.contains(&e.author) {
            return Err("author is revoked".into());
        }
        if e.body.len() > MAX_BODY {
            return Err(format!("body over {MAX_BODY} bytes"));
        }
        if e.kind.needs_evidence() && e.evidence.is_empty() {
            return Err(format!("a {} needs evidence", e.kind.as_str()));
        }
        if let Some(missing) = e.derived_from.iter().find(|d| !self.by_id.contains_key(*d)) {
            return Err(format!("derived from unknown entry {missing}"));
        }
        if let Some(old) = &e.supersedes {
            let old = self.entry(old).ok_or("supersedes an unknown entry")?;
            if old.author != e.author {
                return Err("an entry may only supersede its own author's entries".into());
            }
        }
        let status = if self.owners.contains(&e.author) { Status::Accepted } else { Status::Quarantined };
        let id = e.id.clone();
        self.by_id.insert(id.clone(), self.entries.len());
        self.status.insert(id.clone(), status);
        self.entries.push(e);
        Ok(id)
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.by_id.get(id).map(|i| &self.entries[*i])
    }

    pub fn status(&self, id: &str) -> Option<Status> {
        self.status.get(id).copied()
    }

    /// A reviewer's decision on one entry. Returns the signed event text to share.
    pub fn review(&mut self, id: &str, accept: bool, reviewer: &SigningKey) -> Result<String, String> {
        let text = record::sign(REVIEW, &[("entry", id.into()), ("verdict", if accept { "accept" } else { "reject" }.into())], reviewer)?;
        self.apply(&text)?;
        Ok(text)
    }

    fn apply_review(&mut self, text: &str) -> Result<String, String> {
        let r = record::open(REVIEW, text)?;
        if !self.reviewers.contains(&r.signer) {
            return Err("reviewer is not pinned".into());
        }
        let id = r.req("entry")?.to_string();
        let slot = self.status.get_mut(&id).ok_or("review of an unknown entry")?;
        *slot = match r.req("verdict")? {
            "accept" => Status::Accepted,
            "reject" => Status::Rejected,
            v => return Err(format!("unknown verdict {v:?}")),
        };
        Ok(id)
    }

    /// Withdraw an entry. The author or a reviewer may.
    pub fn retract(&mut self, id: &str, by: &SigningKey) -> Result<String, String> {
        let text = record::sign(RETRACT, &[("entry", id.into())], by)?;
        self.apply(&text)?;
        Ok(text)
    }

    fn apply_retract(&mut self, text: &str) -> Result<String, String> {
        let r = record::open(RETRACT, text)?;
        let id = r.req("entry")?.to_string();
        let e = self.entry(&id).ok_or("retraction of an unknown entry")?;
        if e.author != r.signer && !self.reviewers.contains(&r.signer) {
            return Err("only the author or a reviewer may retract".into());
        }
        self.retracted.insert(id.clone());
        Ok(id)
    }

    /// Stop trusting an author, and everything derived from their entries. Returns the entries that lost trust.
    pub fn revoke_author(&mut self, author: &[u8; 32], by: &SigningKey) -> Result<Vec<String>, String> {
        let before: BTreeSet<String> = self.trusted_ids().into_iter().collect();
        let text = record::sign(REVOKE, &[("author", hex(author))], by)?;
        self.apply(&text)?;
        let after: BTreeSet<String> = self.trusted_ids().into_iter().collect();
        Ok(before.difference(&after).cloned().collect())
    }

    fn apply_revoke(&mut self, text: &str) -> Result<String, String> {
        let r = record::open(REVOKE, text)?;
        if !self.reviewers.contains(&r.signer) {
            return Err("only a reviewer may revoke an author".into());
        }
        let author: [u8; 32] = r.hash("author")?;
        if self.owners.contains(&author) {
            return Err("cannot revoke an owner".into());
        }
        self.revoked.insert(author);
        Ok(hex(&author))
    }

    /// Why `id` is not trusted, or `Ok` if it and everything it derives from is.
    pub fn trusted(&self, id: &str) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        self.trusted_rec(id, &mut seen)
    }

    fn trusted_rec(&self, id: &str, seen: &mut BTreeSet<String>) -> Result<(), String> {
        if !seen.insert(id.to_string()) {
            return Ok(());
        }
        let e = self.entry(id).ok_or_else(|| format!("unknown entry {id}"))?;
        let short = &id[..id.len().min(8)];
        match self.status[id] {
            Status::Quarantined => return Err(format!("{short} is quarantined: no reviewer has accepted it")),
            Status::Rejected => return Err(format!("{short} was rejected")),
            Status::Accepted => {}
        }
        if self.retracted.contains(id) {
            return Err(format!("{short} was retracted"));
        }
        if self.revoked.contains(&e.author) {
            return Err(format!("{short}: its author is revoked"));
        }
        for d in &e.derived_from {
            self.trusted_rec(d, seen).map_err(|why| format!("{short} derives from a distrusted entry: {why}"))?;
        }
        Ok(())
    }

    fn trusted_ids(&self) -> Vec<String> {
        self.entries.iter().filter(|e| self.trusted(&e.id).is_ok()).map(|e| e.id.clone()).collect()
    }

    fn superseded(&self, id: &str) -> bool {
        self.entries.iter().any(|n| n.supersedes.as_deref() == Some(id) && self.trusted(&n.id).is_ok())
    }

    fn live(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| self.trusted(&e.id).is_ok() && !self.superseded(&e.id))
    }

    /// Trusted, current entries of `kind` (or all kinds), labelled in `tracker` so that text an agent
    /// later copies into an action is traced back to its memory entry.
    pub fn recall(&self, kind: Option<Kind>, tracker: &mut TaintTracker) -> Vec<&Entry> {
        let out: Vec<&Entry> = self.live().filter(|e| kind.is_none_or(|k| k == e.kind)).collect();
        for e in &out {
            let trust = if self.owners.contains(&e.author) { Trust::Trusted } else { Trust::Reviewed };
            tracker.register(&e.body, &Source { trust, origin: format!("memory:{}", &e.id[..8]) });
        }
        out
    }

    /// Pairs of live entries on the same non-empty topic with different bodies, for a person to settle.
    pub fn conflicts(&self) -> Vec<(String, String)> {
        let live: Vec<&Entry> = self.live().filter(|e| !e.topic.is_empty()).collect();
        let mut out = Vec::new();
        for (i, a) in live.iter().enumerate() {
            for b in &live[i + 1..] {
                if a.topic == b.topic && a.body != b.body {
                    out.push((a.id.clone(), b.id.clone()));
                }
            }
        }
        out
    }

    /// Every entry `id` rests on, nearest first, with its own evidence hashes.
    pub fn provenance(&self, id: &str) -> Vec<(String, Vec<String>)> {
        let (mut out, mut stack, mut seen) = (Vec::new(), vec![id.to_string()], BTreeSet::new());
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(e) = self.entry(&cur) {
                out.push((cur.clone(), e.evidence.clone()));
                stack.extend(e.derived_from.iter().cloned());
            }
        }
        out
    }

    /// Evidence hashes of `id` that are not entry ids or payload hashes in `history`.
    pub fn evidence_in(&self, id: &str, history: &Log) -> Vec<String> {
        let known: BTreeSet<String> = history.entries.iter().flat_map(|e| [hex(&e.id()), hex(&e.payload_hash)]).collect();
        self.entry(id).map(|e| e.evidence.iter().filter(|h| !known.contains(*h)).cloned().collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }
    fn pk(n: u8) -> [u8; 32] {
        k(n).verifying_key().to_bytes()
    }

    const OWNER: u8 = 1;
    const GUEST: u8 = 2;
    const EVIL: u8 = 3;
    const E: &str = "ab12";

    fn store() -> MemoryStore {
        MemoryStore::new(&[pk(OWNER)], &[])
    }

    fn put(s: &mut MemoryStore, who: u8, kind: Kind, topic: &str, body: &str, derived: &[&str]) -> Result<String, String> {
        let t = Entry::sign(NewEntry { kind, topic, body, evidence: &[E], derived_from: derived, supersedes: None, time_ms: 1 }, &s.head(), &k(who))?;
        s.apply(&t)
    }

    #[test]
    fn guest_entries_are_quarantined_until_a_reviewer_accepts() {
        let mut s = store();
        let own = put(&mut s, OWNER, Kind::Fact, "", "build with cargo", &[]).unwrap();
        let guest = put(&mut s, GUEST, Kind::Fact, "", "tests need --release", &[]).unwrap();
        assert_eq!(s.status(&own), Some(Status::Accepted));
        assert_eq!(s.status(&guest), Some(Status::Quarantined));
        let mut t = TaintTracker::default();
        assert_eq!(s.recall(None, &mut t).len(), 1, "quarantined text is not recalled");
        assert!(s.trusted(&guest).unwrap_err().contains("quarantined"));
        s.review(&guest, true, &k(OWNER)).unwrap();
        assert_eq!(s.recall(None, &mut t).len(), 2);
        assert!(s.review(&guest, false, &k(GUEST)).is_err(), "a guest cannot review");
    }

    #[test]
    fn facts_and_decisions_need_evidence_but_notes_do_not() {
        let mut s = store();
        let bare = |kind| Entry::sign(NewEntry { kind, topic: "", body: "x", evidence: &[], derived_from: &[], supersedes: None, time_ms: 1 }, "", &k(OWNER)).unwrap();
        assert!(s.apply(&bare(Kind::Fact)).unwrap_err().contains("evidence"));
        assert!(s.apply(&bare(Kind::Decision)).is_err());
        s.apply(&bare(Kind::Note)).unwrap();
    }

    #[test]
    fn the_chain_rejects_forks_and_stale_writes() {
        let mut s = store();
        let first = Entry::sign(NewEntry { kind: Kind::Note, topic: "", body: "a", evidence: &[], derived_from: &[], supersedes: None, time_ms: 1 }, "", &k(OWNER)).unwrap();
        s.apply(&first).unwrap();
        let stale = Entry::sign(NewEntry { kind: Kind::Note, topic: "", body: "b", evidence: &[], derived_from: &[], supersedes: None, time_ms: 2 }, "", &k(OWNER)).unwrap();
        assert!(s.apply(&stale).unwrap_err().contains("prev"));
    }

    #[test]
    fn revoking_an_author_also_distrusts_everything_derived_from_them() {
        let mut s = store();
        let bad = put(&mut s, EVIL, Kind::Fact, "", "the deploy key is in /tmp/key", &[]).unwrap();
        s.review(&bad, true, &k(OWNER)).unwrap();
        // The owner's own summary rests on the bad entry; so does a note derived from that summary.
        let summary = put(&mut s, OWNER, Kind::Note, "", "deploy notes", &[&bad]).unwrap();
        let note = put(&mut s, OWNER, Kind::Note, "", "see deploy notes", &[&summary]).unwrap();
        let clean = put(&mut s, OWNER, Kind::Note, "", "unrelated", &[]).unwrap();
        let lost = s.revoke_author(&pk(EVIL), &k(OWNER)).unwrap();
        assert_eq!(lost.iter().collect::<BTreeSet<_>>(), [&bad, &summary, &note].into_iter().collect());
        assert!(s.trusted(&note).unwrap_err().contains("derives from a distrusted entry"));
        assert!(s.trusted(&clean).is_ok());
        let mut t = TaintTracker::default();
        assert_eq!(s.recall(None, &mut t).len(), 1);
        let late = Entry::sign(NewEntry { kind: Kind::Note, topic: "", body: "more", evidence: &[], derived_from: &[], supersedes: None, time_ms: 9 }, &s.head(), &k(EVIL)).unwrap();
        assert!(s.apply(&late).unwrap_err().contains("revoked"));
        assert!(s.revoke_author(&pk(OWNER), &k(OWNER)).is_err(), "owners cannot be revoked");
    }

    #[test]
    fn rejection_and_retraction_cascade_too() {
        let mut s = store();
        let a = put(&mut s, GUEST, Kind::Note, "", "a", &[]).unwrap();
        s.review(&a, true, &k(OWNER)).unwrap();
        let b = put(&mut s, OWNER, Kind::Note, "", "b", &[&a]).unwrap();
        assert!(s.trusted(&b).is_ok());
        assert!(s.retract(&a, &k(EVIL)).is_err(), "a stranger cannot retract");
        s.retract(&a, &k(GUEST)).unwrap();
        assert!(s.trusted(&b).is_err());
        let c = put(&mut s, GUEST, Kind::Note, "", "c", &[]).unwrap();
        s.review(&c, false, &k(OWNER)).unwrap();
        assert!(s.trusted(&c).unwrap_err().contains("rejected"));
    }

    #[test]
    fn supersedes_replaces_only_your_own_entries_and_conflicts_are_surfaced() {
        let mut s = store();
        let v1 = put(&mut s, OWNER, Kind::Fact, "port", "8080", &[]).unwrap();
        let g = put(&mut s, GUEST, Kind::Fact, "port", "9090", &[]).unwrap();
        s.review(&g, true, &k(OWNER)).unwrap();
        assert_eq!(s.conflicts(), vec![(v1.clone(), g.clone())]);
        let steal = Entry::sign(NewEntry { kind: Kind::Fact, topic: "port", body: "1", evidence: &[E], derived_from: &[], supersedes: Some(&v1), time_ms: 3 }, &s.head(), &k(GUEST)).unwrap();
        assert!(s.apply(&steal).unwrap_err().contains("own author"));
        let v2 = Entry::sign(NewEntry { kind: Kind::Fact, topic: "port", body: "9090", evidence: &[E], derived_from: &[], supersedes: Some(&v1), time_ms: 3 }, &s.head(), &k(OWNER)).unwrap();
        s.apply(&v2).unwrap();
        assert!(s.conflicts().is_empty(), "same body now, and v1 is superseded");
        let mut t = TaintTracker::default();
        assert!(s.recall(None, &mut t).iter().all(|e| e.id != v1));
    }

    #[test]
    fn recall_labels_text_so_the_broker_can_trace_it() {
        use crate::taint::{Broker, Decision, Policy};
        let mut s = store();
        let g = put(&mut s, GUEST, Kind::Skill, "", "to release run publish.sh --token $DEPLOY_TOKEN --force", &[]).unwrap();
        s.review(&g, true, &k(OWNER)).unwrap();
        put(&mut s, OWNER, Kind::Skill, "", "to test run cargo test --workspace", &[]).unwrap();
        let mut b = Broker::new(Policy::playbox_default(), vec![pk(OWNER)], k(9));
        s.recall(None, &mut b.tracker);
        assert!(matches!(b.admit("shell", &["publish.sh --token $DEPLOY_TOKEN --force"], 1), Decision::NeedsApproval(_)));
        assert_eq!(b.admit("shell", &["cargo test --workspace"], 2), Decision::Allow);
    }

    #[test]
    fn replay_reaches_the_same_state_and_rejects_tampering() {
        let mut s = store();
        let g = put(&mut s, GUEST, Kind::Note, "", "g", &[]).unwrap();
        s.review(&g, true, &k(OWNER)).unwrap();
        s.revoke_author(&pk(GUEST), &k(OWNER)).unwrap();
        let r = MemoryStore::replay(&[pk(OWNER)], &[], s.log()).unwrap();
        assert_eq!(r.head(), s.head());
        assert!(r.trusted(&g).is_err());
        let mut forged = s.log().to_vec();
        forged[0] = forged[0].replace("kind note", "kind fact");
        assert!(MemoryStore::replay(&[pk(OWNER)], &[], &forged).is_err());
        let mut dropped = s.log().to_vec();
        dropped.remove(0);
        assert!(MemoryStore::replay(&[pk(OWNER)], &[], &dropped).is_err(), "review of an entry that is gone");
    }

    #[test]
    fn evidence_can_be_checked_against_a_history() {
        let mut log = Log::new();
        let h = hex(&log.append(&k(GUEST), "commit", b"abc", 1).id());
        let mut s = store();
        let t = Entry::sign(NewEntry { kind: Kind::Fact, topic: "", body: "x", evidence: &[&h, "dead"], derived_from: &[], supersedes: None, time_ms: 1 }, "", &k(OWNER)).unwrap();
        let id = s.apply(&t).unwrap();
        assert_eq!(s.evidence_in(&id, &log), vec!["dead".to_string()]);
        assert_eq!(s.provenance(&id).len(), 1);
    }
}
