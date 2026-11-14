//! Conversations: threads with a roster, kept in order by the host, whose entries the host cannot forge.
//!
//! This is the first slice of Peerspan's conversations (home host, roster version, per-author sequence, read marks, rate limits),
//! with one addition: **every entry carries the author's own Ed25519 signature** over the conversation, the request id and the text.
//! The host orders and stores entries and checks the signature before accepting it, but a reader can check it again, so a host
//! (or a replica) cannot put words in a member's mouth. The host's audit log records each post, so it cannot silently drop or
//! reorder them either.
//!
//! * A conversation has a host-assigned id, a title, a creator and a roster. Only roster members read or post. The creator can add
//!   members (each must be a current member of the room); the roster version increases on every change. Nobody is removed here;
//!   revoking a member of the room ends their access to every conversation.
//! * Posts are numbered 1, 2, … per conversation. `since` reads entries after a number. A post with the same `request_id` from the
//!   same author returns the first post's number (a retry after a lost reply says nothing twice); the same id with other text is refused.
//! * Read marks are each member's own cursor, so "unread" survives reconnects and restarts.
//! * Bounds: 4096 bytes per entry, 2 to 32 roster members, 100 conversations per creator, 30 posts a minute per author, 10 000
//!   entries per conversation (the oldest are dropped from the front; sequence numbers continue).
//! * Entries are text from other members. Whatever shows them to an agent must treat them as untrusted (the MCP tools do).

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

pub const MAX_TEXT: usize = 4096;
pub const MAX_ROSTER: usize = 32;
pub const MAX_PER_CREATOR: usize = 100;
pub const MAX_ENTRIES: usize = 10_000;
pub const POSTS_PER_MINUTE: usize = 30;
const TITLE_MAX: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub author: String,
    pub ts_ms: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Ed25519 signature by `author` over [`signed_text`], hex.
    pub sig: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub creator: String,
    pub roster: Vec<String>,
    pub roster_version: u64,
    pub created_ms: u64,
    pub next_seq: u64,
    pub entries: VecDeque<Entry>,
    /// Each member's last read entry.
    pub marks: BTreeMap<String, u64>,
}

/// What an author signs. A different conversation, request id or text gives a different message.
#[must_use]
pub fn signed_text(conv: &str, request_id: &str, text: &str) -> Vec<u8> {
    format!("playbox.conv-entry.v1\n{conv}\n{request_id}\n{}\n{text}", text.len()).into_bytes()
}

/// Check `sig` (hex) as `author`'s (a 32-byte public key, hex or the bytes themselves given as `key`).
fn verify(key: &[u8; 32], conv: &str, request_id: &str, text: &str, sig: &str) -> Result<()> {
    let raw = (0..sig.len() / 2).map(|i| u8::from_str_radix(sig.get(2 * i..2 * i + 2).unwrap_or("zz"), 16)).collect::<std::result::Result<Vec<u8>, _>>().context("signature is not hex")?;
    let sig: [u8; 64] = raw.try_into().map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    VerifyingKey::from_bytes(key)?.verify(&signed_text(conv, request_id, text), &Signature::from_bytes(&sig)).context("the signature does not match the author")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub struct Book {
    inner: Mutex<Inner>,
    store: Option<PathBuf>,
}

#[derive(Default)]
struct Inner {
    convs: BTreeMap<String, Conversation>,
    /// Recent post times per author, for the rate limit. Not persisted.
    recent: BTreeMap<String, VecDeque<u64>>,
}

/// What a read returns.
#[derive(Debug, Serialize)]
pub struct View {
    pub id: String,
    pub title: String,
    pub roster: Vec<String>,
    pub roster_version: u64,
    pub entries: Vec<Entry>,
    /// The reader's own mark, and how many entries after it exist.
    pub mark: u64,
    pub unread: u64,
    pub last_seq: u64,
}

impl Book {
    /// # Errors
    ///
    /// Fails if the store exists but cannot be read.
    pub fn new(store: Option<PathBuf>) -> Result<Self> {
        let mut inner = Inner::default();
        if let Some(p) = &store {
            if let Ok(text) = std::fs::read_to_string(p) {
                let list: Vec<Conversation> = serde_json::from_str(&text).with_context(|| format!("reading {}", p.display()))?;
                for c in list {
                    inner.convs.insert(c.id.clone(), c);
                }
            }
        }
        Ok(Self { inner: Mutex::new(inner), store })
    }

    fn save(&self, i: &Inner) {
        if let Some(p) = &self.store {
            if let Ok(text) = serde_json::to_string(&i.convs.values().collect::<Vec<_>>()) {
                if let Err(e) = crate::write_atomic(p, text.as_bytes()) {
                    eprintln!("playbox: cannot save the conversations {}: {e}", p.display());
                }
            }
        }
    }

    /// Start a conversation. `members` are added to `creator`; `is_member` says whether an id belongs to a current room member.
    ///
    /// # Errors
    ///
    /// Refuses a bad title, a roster that is too small or large, an unknown member, or a creator with too many conversations.
    pub fn create(&self, creator: &str, title: &str, members: &[String], is_member: &dyn Fn(&str) -> bool) -> Result<Conversation> {
        let title = title.trim();
        if title.is_empty() || title.len() > TITLE_MAX || title.chars().any(char::is_control) {
            bail!("a title is 1 to {TITLE_MAX} characters without control characters");
        }
        let mut roster = vec![creator.to_string()];
        for m in members {
            if !is_member(m) {
                bail!("{m} is not a member of this room");
            }
            if !roster.contains(m) {
                roster.push(m.clone());
            }
        }
        if roster.len() < 2 || roster.len() > MAX_ROSTER {
            bail!("a conversation has 2 to {MAX_ROSTER} members");
        }
        let mut i = self.inner.lock().expect("conversations lock");
        if i.convs.values().filter(|c| c.creator == creator).count() >= MAX_PER_CREATOR {
            bail!("too many conversations ({MAX_PER_CREATOR})");
        }
        let mut rnd = [0u8; 8];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut rnd)?;
        let id = format!("c-{}", rnd.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let c = Conversation { id: id.clone(), title: title.to_string(), creator: creator.to_string(), roster, roster_version: 1, created_ms: now_ms(), next_seq: 1, entries: VecDeque::new(), marks: BTreeMap::new() };
        i.convs.insert(id, c.clone());
        self.save(&i);
        Ok(c)
    }

    /// Add members to a conversation (creator only).
    ///
    /// # Errors
    ///
    /// Refuses anyone but the creator, an unknown member, or a roster over the limit.
    pub fn invite(&self, who: &str, conv: &str, members: &[String], is_member: &dyn Fn(&str) -> bool) -> Result<Conversation> {
        let mut i = self.inner.lock().expect("conversations lock");
        let c = i.convs.get_mut(conv).filter(|c| c.roster.iter().any(|m| m == who)).context("no such conversation")?;
        if c.creator != who {
            bail!("only the creator can add members");
        }
        // Work out the new roster first: a refused invite must leave the conversation as it was.
        let mut roster = c.roster.clone();
        for m in members {
            if !is_member(m) {
                bail!("{m} is not a member of this room");
            }
            if !roster.contains(m) {
                roster.push(m.clone());
            }
        }
        if roster.len() > MAX_ROSTER {
            bail!("a conversation has at most {MAX_ROSTER} members");
        }
        if roster.len() != c.roster.len() {
            c.roster = roster;
            c.roster_version += 1;
        }
        let out = c.clone();
        self.save(&i);
        Ok(out)
    }

    /// Post an entry. `author_key` is the author's public key; `sig` signs [`signed_text`].
    ///
    /// # Errors
    ///
    /// Refuses a non-roster author, a bad signature, text that is empty or too long, a rate overrun, or a reused request id with other text.
    pub fn post(&self, author: &str, author_key: &[u8; 32], conv: &str, text: &str, request_id: &str, sig: &str) -> Result<Entry> {
        if text.trim().is_empty() || text.len() > MAX_TEXT {
            bail!("an entry is 1 to {MAX_TEXT} bytes");
        }
        if !valid_id(request_id) {
            bail!("request_id must be 1-64 letters, digits, - _ .");
        }
        let mut i = self.inner.lock().expect("conversations lock");
        let c = i.convs.get(conv).filter(|c| c.roster.iter().any(|m| m == author)).context("no such conversation")?;
        if let Some(prev) = c.entries.iter().find(|e| e.author == author && e.request_id.as_deref() == Some(request_id)) {
            if prev.text == text {
                return Ok(prev.clone());
            }
            bail!("request_id {request_id:?} was already used for other text");
        }
        verify(author_key, conv, request_id, text, sig)?;
        let now = now_ms();
        let q = i.recent.entry(author.to_string()).or_default();
        while q.front().is_some_and(|t| now.saturating_sub(*t) > 60_000) {
            q.pop_front();
        }
        if q.len() >= POSTS_PER_MINUTE {
            bail!("too many posts: at most {POSTS_PER_MINUTE} a minute");
        }
        q.push_back(now);
        let c = i.convs.get_mut(conv).expect("checked above");
        let e = Entry { seq: c.next_seq, author: author.to_string(), ts_ms: now, text: text.to_string(), request_id: Some(request_id.to_string()), sig: sig.to_string() };
        c.next_seq += 1;
        c.entries.push_back(e.clone());
        while c.entries.len() > MAX_ENTRIES {
            c.entries.pop_front();
        }
        self.save(&i);
        Ok(e)
    }

    /// Read a conversation after `since` (at most 200 entries) and move the reader's mark to the last entry returned.
    ///
    /// # Errors
    ///
    /// Refuses a reader who is not on the roster.
    pub fn read(&self, who: &str, conv: &str, since: u64, mark: bool) -> Result<View> {
        let mut i = self.inner.lock().expect("conversations lock");
        let c = i.convs.get_mut(conv).filter(|c| c.roster.iter().any(|m| m == who)).context("no such conversation")?;
        let entries: Vec<Entry> = c.entries.iter().filter(|e| e.seq > since).take(200).cloned().collect();
        let last_seq = c.next_seq - 1;
        if mark {
            if let Some(e) = entries.last() {
                let m = c.marks.entry(who.to_string()).or_insert(0);
                *m = (*m).max(e.seq);
            }
        }
        let mk = c.marks.get(who).copied().unwrap_or(0);
        let view = View { id: c.id.clone(), title: c.title.clone(), roster: c.roster.clone(), roster_version: c.roster_version, entries, mark: mk, unread: c.entries.iter().filter(|e| e.seq > mk && e.author != who).count() as u64, last_seq };
        if mark {
            self.save(&i);
        }
        Ok(view)
    }

    /// The conversations `who` is on, with unread counts.
    #[must_use]
    pub fn list(&self, who: &str) -> Value {
        let i = self.inner.lock().expect("conversations lock");
        json!(i
            .convs
            .values()
            .filter(|c| c.roster.iter().any(|m| m == who))
            .map(|c| {
                let mk = c.marks.get(who).copied().unwrap_or(0);
                json!({"id": c.id, "title": c.title, "creator": c.creator, "roster": c.roster, "roster_version": c.roster_version,
                       "last_seq": c.next_seq - 1, "unread": c.entries.iter().filter(|e| e.seq > mk && e.author != who).count()})
            })
            .collect::<Vec<_>>())
    }

    /// Entries after each member's mark across all their conversations, for a live feed: (conversation id, title, entry).
    #[must_use]
    pub fn unread_for(&self, who: &str, after: &BTreeMap<String, u64>) -> Vec<(String, String, Entry)> {
        let i = self.inner.lock().expect("conversations lock");
        let mut out = Vec::new();
        for c in i.convs.values().filter(|c| c.roster.iter().any(|m| m == who)) {
            let from = after.get(&c.id).copied().unwrap_or(0);
            out.extend(c.entries.iter().filter(|e| e.seq > from && e.author != who).take(50).map(|e| (c.id.clone(), c.title.clone(), e.clone())));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key(n: u8) -> (SigningKey, String, [u8; 32]) {
        let k = SigningKey::from_bytes(&[n; 32]);
        let pk = k.verifying_key().to_bytes();
        (k, pk.iter().map(|b| format!("{b:02x}")).collect(), pk)
    }

    fn sign(k: &SigningKey, conv: &str, rid: &str, text: &str) -> String {
        k.sign(&signed_text(conv, rid, text)).to_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn members<'a>(ids: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |m| ids.contains(&m)
    }

    #[test]
    fn a_roster_member_posts_signed_entries_in_order_and_others_cannot_read() {
        let b = Book::new(None).unwrap();
        let (ka, a, apk) = key(1);
        let (kb, bb, bpk) = key(2);
        let (_kc, c, _) = key(3);
        let ids = [a.as_str(), bb.as_str(), c.as_str()];
        let conv = b.create(&a, "plan", &[bb.clone()], &members(&ids)).unwrap();
        assert_eq!((conv.roster.len(), conv.roster_version), (2, 1));
        let e1 = b.post(&a, &apk, &conv.id, "hello", "r1", &sign(&ka, &conv.id, "r1", "hello")).unwrap();
        let e2 = b.post(&bb, &bpk, &conv.id, "hi", "r1", &sign(&kb, &conv.id, "r1", "hi")).unwrap();
        assert_eq!((e1.seq, e2.seq), (1, 2), "request ids are per author");
        assert!(b.read(&c, &conv.id, 0, false).is_err(), "a non-member sees nothing");
        assert!(b.post(&c, &key(3).2, &conv.id, "x", "r", &sign(&key(3).0, &conv.id, "r", "x")).is_err());
        let v = b.read(&bb, &conv.id, 0, true).unwrap();
        assert_eq!((v.entries.len(), v.mark, v.unread), (2, 2, 0));
        assert_eq!(b.list(&a)[0]["unread"], 1, "a has not read b's entry");
    }

    #[test]
    fn an_invite_that_overflows_the_roster_changes_nothing() {
        let b = Book::new(None).unwrap();
        let everyone: Vec<String> = (0..MAX_ROSTER + 8).map(|i| format!("m{i}")).collect();
        let known = |m: &str| everyone.iter().any(|e| e == m);
        let conv = b.create("m0", "t", &["m1".to_string()], &known).unwrap();
        assert!(b.invite("m0", &conv.id, &everyone, &known).is_err());
        let v = b.read("m0", &conv.id, 0, false).unwrap();
        assert_eq!((v.roster.len(), v.roster_version), (2, 1), "a refused invite must not leave members behind");
    }

    #[test]
    fn the_host_cannot_forge_an_entry() {
        let b = Book::new(None).unwrap();
        let (ka, a, apk) = key(1);
        let (_kb, bb, bpk) = key(2);
        let ids = [a.as_str(), bb.as_str()];
        let conv = b.create(&a, "t", &[bb.clone()], &members(&ids)).unwrap();
        let good = sign(&ka, &conv.id, "r1", "hello");
        // Someone else's key, altered text, another conversation, another request id.
        assert!(b.post(&bb, &bpk, &conv.id, "hello", "r1", &good).is_err());
        assert!(b.post(&a, &apk, &conv.id, "hello!", "r1", &good).is_err());
        assert!(b.post(&a, &apk, &conv.id, "hello", "r2", &good).is_err());
        assert!(b.post(&a, &apk, &conv.id, "hello", "r1", "00").is_err());
        assert!(b.post(&a, &apk, &conv.id, "hello", "r1", &"zz".repeat(64)).is_err());
        let e = b.post(&a, &apk, &conv.id, "hello", "r1", &good).unwrap();
        // A reader can verify the stored entry on its own.
        assert!(verify(&apk, &conv.id, "r1", &e.text, &e.sig).is_ok());
    }

    #[test]
    fn a_retried_post_is_not_posted_twice() {
        let b = Book::new(None).unwrap();
        let (ka, a, apk) = key(1);
        let (_kb, bb, _) = key(2);
        let ids = [a.as_str(), bb.as_str()];
        let conv = b.create(&a, "t", &[bb.clone()], &members(&ids)).unwrap();
        let s = sign(&ka, &conv.id, "r1", "once");
        let first = b.post(&a, &apk, &conv.id, "once", "r1", &s).unwrap();
        let again = b.post(&a, &apk, &conv.id, "once", "r1", &s).unwrap();
        assert_eq!(first, again);
        assert_eq!(b.read(&a, &conv.id, 0, false).unwrap().entries.len(), 1);
        let other = sign(&ka, &conv.id, "r1", "different");
        assert!(b.post(&a, &apk, &conv.id, "different", "r1", &other).is_err());
    }

    #[test]
    fn bounds_and_rate_limit() {
        let b = Book::new(None).unwrap();
        let (ka, a, apk) = key(1);
        let (_kb, bb, _) = key(2);
        let ids = [a.as_str(), bb.as_str()];
        assert!(b.create(&a, "", &[bb.clone()], &members(&ids)).is_err());
        assert!(b.create(&a, "solo", &[], &members(&ids)).is_err(), "needs a second member");
        assert!(b.create(&a, "t", &["stranger".into()], &members(&ids)).is_err());
        let conv = b.create(&a, "t", &[bb.clone()], &members(&ids)).unwrap();
        let big = "x".repeat(MAX_TEXT + 1);
        assert!(b.post(&a, &apk, &conv.id, &big, "r", &sign(&ka, &conv.id, "r", &big)).is_err());
        for n in 0..POSTS_PER_MINUTE {
            let rid = format!("p{n}");
            b.post(&a, &apk, &conv.id, "x", &rid, &sign(&ka, &conv.id, &rid, "x")).unwrap();
        }
        assert!(b.post(&a, &apk, &conv.id, "x", "over", &sign(&ka, &conv.id, "over", "x")).is_err());
    }

    #[test]
    fn only_the_creator_adds_members_and_the_version_moves() {
        let b = Book::new(None).unwrap();
        let (_ka, a, _) = key(1);
        let (_kb, bb, _) = key(2);
        let (_kc, c, _) = key(3);
        let ids = [a.as_str(), bb.as_str(), c.as_str()];
        let conv = b.create(&a, "t", &[bb.clone()], &members(&ids)).unwrap();
        assert!(b.invite(&bb, &conv.id, &[c.clone()], &members(&ids)).is_err());
        let v2 = b.invite(&a, &conv.id, &[c.clone()], &members(&ids)).unwrap();
        assert_eq!((v2.roster.len(), v2.roster_version), (3, 2));
        assert_eq!(b.invite(&a, &conv.id, &[c.clone()], &members(&ids)).unwrap().roster_version, 2, "adding a present member changes nothing");
    }

    #[test]
    fn conversations_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("playbox-conv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("conversations.json");
        let _ = std::fs::remove_file(&store);
        let (ka, a, apk) = key(1);
        let (_kb, bb, _) = key(2);
        let ids = [a.as_str(), bb.as_str()];
        let id = {
            let b = Book::new(Some(store.clone())).unwrap();
            let conv = b.create(&a, "keep", &[bb.clone()], &members(&ids)).unwrap();
            b.post(&a, &apk, &conv.id, "kept", "r1", &sign(&ka, &conv.id, "r1", "kept")).unwrap();
            conv.id
        };
        let b = Book::new(Some(store)).unwrap();
        let v = b.read(&bb, &id, 0, true).unwrap();
        assert_eq!(v.entries[0].text, "kept");
        // The sequence continues and the retry is still recognised after the restart.
        let again = b.post(&a, &apk, &id, "kept", "r1", &sign(&ka, &id, "r1", "kept")).unwrap();
        assert_eq!(again.seq, 1);
        let next = b.post(&a, &apk, &id, "more", "r2", &sign(&ka, &id, "r2", "more")).unwrap();
        assert_eq!(next.seq, 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
