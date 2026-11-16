//! A shared memory for the room, kept by a Hindsight sidecar (docs/latent-channels.md, section 6o).
//!
//! The host is the only client of the memory server. A member posts an entry with `remember` and asks with `recall`; the host
//! stamps what the server cannot be trusted to know (who wrote it, in which room, whether it was reviewed) and filters what comes
//! back. The server is a search index; the host's own ledger (`memory.jsonl`) is the record.
//!
//! * Every entry is a document `m-<hash of author and key>`: posting the same text (or the same `key`) again by the same
//!   member replaces it instead of duplicating, so a retry after a lost reply is safe.
//! * Tags `room:<id>`, `author:<member>` and `status:<state>` go with every entry. Recall asks for `status:accepted` of this
//!   room, and then keeps only hits whose document is in the ledger, is accepted there, and whose author is still a member.
//!   A claim the server mislabels, or one from a revoked member, is dropped here.
//! * With `review` on, a post waits in the ledger as `pending` and never reaches the server until the host accepts it, so
//!   unreviewed text cannot be consolidated with anything. `quarantine` removes an entry from the index and keeps the record.
//! * Revoking a member deletes their documents from the server (`forget_author`); a sweep before each recall finishes any
//!   deletion that failed the first time.
//! * Recall never returns derived "observations": they merge several entries and the server records no author for the result.
//!   Whether consolidation can be kept apart by author is unproven (6o), so the bridge does not rely on it.

use crate::sync::LockExt;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod backend;
mod hindsight;
#[cfg(test)]
mod tests;
pub use backend::{Backend, InMemory, Item, RawHit, RecallRequest, Status};
pub(crate) use hindsight::parse_response;
pub use hindsight::Hindsight;

pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_KEY_BYTES: usize = 64;
pub const MAX_QUERY_BYTES: usize = 1024;
pub const MAX_RECALL_TOKENS: u32 = 4000;
pub const DEFAULT_RECALL_TOKENS: u32 = 1500;
const MAX_RESPONSE_BYTES: usize = 8 << 20;

#[derive(Clone)]
pub struct MemoryConfig {
    /// The index. Often [`Hindsight`].
    pub backend: std::sync::Arc<dyn Backend>,
    /// The room's bank on the server and the `room:` tag. Letters, digits, `-` and `_`.
    pub room: String,
    /// Keep a member's post as `pending` until the host accepts it. Default false: posts are accepted at once.
    pub review: bool,
    /// Where the ledger is kept. `None`: memory only.
    pub ledger: Option<PathBuf>,
    pub max_entries: usize,
    pub max_entries_per_author: usize,
    pub posts_per_minute: u32,
}

impl MemoryConfig {
    #[must_use]
    pub fn new(backend: std::sync::Arc<dyn Backend>, room: &str) -> Self {
        Self {
            backend,
            room: room.to_string(),
            review: false,
            ledger: None,
            max_entries: 10_000,
            max_entries_per_author: 1_000,
            posts_per_minute: 30,
        }
    }
}

/// A room's memory on a Hindsight sidecar at `url`: `bank` names the room's bank, and the ledger lives in `dir` when the host has a state directory.
///
/// # Errors
///
/// Returns an error if the URL or bank name is not acceptable.
pub fn hindsight_memory(
    url: &str,
    bank: &str,
    review: bool,
    wait: bool,
    dir: Option<&std::path::Path>,
) -> Result<std::sync::Arc<SharedMemory>> {
    let mut h = Hindsight::new(url)?;
    h.wait = wait;
    let mut c = MemoryConfig::new(std::sync::Arc::new(h), bank);
    c.review = review;
    c.ledger = dir.map(|d| d.join("memory.jsonl"));
    Ok(std::sync::Arc::new(SharedMemory::new(c)?))
}

/// One entry in the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub doc: String,
    pub author: String,
    pub text: String,
    pub status: Status,
    pub at_ms: u64,
}

/// What a recall returns for one hit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hit {
    pub doc: String,
    pub author: String,
    pub text: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Remembered {
    pub doc: String,
    pub status: Status,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Line {
    Put(Entry),
    Status { doc: String, status: Status },
    Del { doc: String },
}

pub struct SharedMemory {
    cfg: MemoryConfig,
    ledger: Mutex<BTreeMap<String, Entry>>,
    posts: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// One writer at a time to the ledger file and the index, so an entry and its deletion cannot cross.
    io: tokio::sync::Mutex<()>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().try_into().unwrap_or(u64::MAX))
}

/// The document id for an entry: the same author and key (or text) always give the same one.
#[must_use]
pub fn doc_id(author: &str, key_or_text: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"playbox-memory\0");
    h.update(author.as_bytes());
    h.update(b"\0");
    h.update(key_or_text.as_bytes());
    format!("m-{}", &h.finalize().to_hex()[..32])
}

impl SharedMemory {
    /// # Errors
    ///
    /// Returns an error if the room name is not a plain name, or if the ledger exists but cannot be read.
    pub fn new(cfg: MemoryConfig) -> Result<Self> {
        if cfg.room.is_empty()
            || cfg.room.len() > 64
            || !cfg
                .room
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("the room's memory name must be 1 to 64 letters, digits, - or _");
        }
        let mut ledger = BTreeMap::new();
        if let Some(p) = &cfg.ledger {
            // Bytes, not a string: a crash can cut the last line inside a multi-byte character, and that must cost
            // that one line, not the room's whole memory.
            if let Ok(bytes) = std::fs::read(p) {
                if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                    // End the torn line, or the next entry would be glued to it and lost with it.
                    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(p) {
                        let _ = f.write_all(b"\n");
                    }
                }
                let text = String::from_utf8_lossy(&bytes);
                for l in text.lines() {
                    // A damaged line is skipped, like the feed's log: one bad write must not lose the room's memory.
                    match serde_json::from_str::<Line>(l) {
                        Ok(Line::Put(e)) => {
                            ledger.insert(e.doc.clone(), e);
                        }
                        Ok(Line::Status { doc, status }) => {
                            if let Some(e) = ledger.get_mut(&doc) {
                                e.status = status;
                            }
                        }
                        Ok(Line::Del { doc }) => {
                            ledger.remove(&doc);
                        }
                        Err(_) => {}
                    }
                }
            }
        }
        Ok(Self {
            cfg,
            ledger: Mutex::new(ledger),
            posts: Mutex::default(),
            io: tokio::sync::Mutex::new(()),
        })
    }

    fn append(&self, line: &Line) -> Result<()> {
        let Some(p) = &self.cfg.ledger else {
            return Ok(());
        };
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .context("open the memory ledger")?;
        let mut s = serde_json::to_string(line)?;
        s.push('\n');
        f.write_all(s.as_bytes())
            .context("write the memory ledger")?;
        f.sync_data().ok();
        Ok(())
    }

    fn tags(&self, e: &Entry) -> Vec<String> {
        vec![
            format!("room:{}", self.cfg.room),
            format!("author:{}", e.author),
            format!("status:{}", e.status.as_str()),
        ]
    }

    fn item(&self, e: &Entry) -> Item {
        let metadata = BTreeMap::from([
            ("author_key".to_string(), e.author.clone()),
            ("status".to_string(), e.status.as_str().to_string()),
            ("posted_ms".to_string(), e.at_ms.to_string()),
        ]);
        Item {
            doc: e.doc.clone(),
            text: e.text.clone(),
            tags: self.tags(e),
            metadata,
        }
    }

    fn rate_ok(&self, author: &str) -> bool {
        let mut g = self.posts.lock_recover();
        let q = g.entry(author.to_string()).or_default();
        let now = Instant::now();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            q.pop_front();
        }
        if q.len() >= self.cfg.posts_per_minute as usize {
            return false;
        }
        q.push_back(now);
        true
    }

    /// Post an entry for `author` (a member's id). `key` names the entry so that a later post with the same key replaces it.
    ///
    /// # Errors
    ///
    /// Returns a message for the member if the entry is refused or could not be stored.
    pub async fn remember(
        &self,
        author: &str,
        text: &str,
        key: Option<&str>,
    ) -> std::result::Result<Remembered, String> {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
            return Err(format!("an entry is 1 to {MAX_TEXT_BYTES} bytes of text"));
        }
        if let Some(k) = key {
            if k.is_empty() || k.len() > MAX_KEY_BYTES {
                return Err(format!("a key is 1 to {MAX_KEY_BYTES} bytes"));
            }
        }
        let doc = doc_id(author, key.unwrap_or(text));
        let status = if self.cfg.review {
            Status::Pending
        } else {
            Status::Accepted
        };
        let entry = Entry {
            doc: doc.clone(),
            author: author.to_string(),
            text: text.to_string(),
            status,
            at_ms: now_ms(),
        };
        let _io = self.io.lock().await;
        let (replaces, same) = {
            let g = self.ledger.lock_recover();
            let old = g.get(&doc);
            if old.is_none() {
                if g.len() >= self.cfg.max_entries {
                    return Err("the room's memory is full".into());
                }
                if g.values().filter(|e| e.author == author).count()
                    >= self.cfg.max_entries_per_author
                {
                    return Err("you have as many entries as one member may keep".into());
                }
            }
            (
                old.cloned(),
                old.is_some_and(|o| o.text == text && o.status != Status::Quarantined),
            )
        };
        // A retry of the same entry is answered without work, and without using the member's post budget.
        if same {
            return Ok(Remembered {
                doc,
                status: replaces.map_or(status, |o| o.status),
            });
        }
        if replaces
            .as_ref()
            .is_some_and(|o| o.status == Status::Quarantined)
        {
            return Err("that entry was quarantined by the host".into());
        }
        if !self.rate_ok(author) {
            return Err("too many entries this minute".into());
        }
        self.append(&Line::Put(entry.clone()))
            .map_err(|e| format!("{e:#}"))?;
        if status == Status::Accepted {
            if let Err(e) = self
                .cfg
                .backend
                .retain(&self.cfg.room, &self.item(&entry))
                .await
            {
                // Undo, so the member's retry starts clean instead of finding an entry that is not in the index.
                let undo = match &replaces {
                    Some(o) => Line::Put(o.clone()),
                    None => Line::Del { doc: doc.clone() },
                };
                self.append(&undo).ok();
                return Err(format!("{e:#}"));
            }
        }
        self.ledger.lock_recover().insert(doc.clone(), entry);
        Ok(Remembered { doc, status })
    }

    /// Entries the room's members posted that match `query`: accepted, from a current member, newest information first as the server ranks it.
    /// `is_member` says whether a key belongs to a current member of the room.
    ///
    /// # Errors
    ///
    /// Returns a message for the member if the query is refused or the index cannot answer.
    pub async fn recall(
        &self,
        query: &str,
        max_tokens: Option<u32>,
        author: Option<&str>,
        is_member: &(dyn Fn(&str) -> bool + Sync),
    ) -> std::result::Result<Vec<Hit>, String> {
        if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES {
            return Err(format!("a query is 1 to {MAX_QUERY_BYTES} bytes of text"));
        }
        // Finish any revocation that did not complete, before anything is read.
        self.sweep(is_member).await;
        let mut tags = vec![
            format!("room:{}", self.cfg.room),
            "status:accepted".to_string(),
        ];
        if let Some(a) = author {
            tags.push(format!("author:{a}"));
        }
        let req = RecallRequest {
            query: query.to_string(),
            max_tokens: max_tokens
                .unwrap_or(DEFAULT_RECALL_TOKENS)
                .clamp(100, MAX_RECALL_TOKENS),
            tags,
        };
        let raw = self
            .cfg
            .backend
            .recall(&self.cfg.room, &req)
            .await
            .map_err(|e| format!("{e:#}"))?;
        let g = self.ledger.lock_recover();
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for h in raw {
            // Derived memories merge entries and record no author; entries the server cannot attribute are not shown.
            if h.kind.as_deref() == Some("observation") {
                continue;
            }
            let Some(doc) = h.doc else { continue };
            let Some(e) = g.get(&doc) else { continue };
            if e.status != Status::Accepted
                || !is_member(&e.author)
                || author.is_some_and(|a| a != e.author)
            {
                continue;
            }
            // The tags the server returned must agree with the ledger, or the index was written by something else.
            if !h.tags.contains(&format!("author:{}", e.author))
                || !h.tags.contains(&format!("room:{}", self.cfg.room))
            {
                continue;
            }
            // One hit per document is enough for an agent reading context; the server may return several facts of one entry.
            if !seen.insert(doc.clone()) {
                continue;
            }
            out.push(Hit {
                doc,
                author: e.author.clone(),
                text: h.text,
                at_ms: e.at_ms,
            });
        }
        Ok(out)
    }

    /// Delete every document of `author` from the index and the ledger. Returns how many were removed.
    /// Call it when a member is revoked; a document the server would not delete stays in the ledger and is retried.
    pub async fn forget_author(&self, author: &str) -> usize {
        let _io = self.io.lock().await;
        let docs: Vec<String> = self
            .ledger
            .lock_recover()
            .values()
            .filter(|e| e.author == author)
            .map(|e| e.doc.clone())
            .collect();
        let mut n = 0;
        for d in docs {
            if self.cfg.backend.delete(&self.cfg.room, &d).await.is_ok()
                && self.append(&Line::Del { doc: d.clone() }).is_ok()
            {
                self.ledger.lock_recover().remove(&d);
                n += 1;
            }
        }
        n
    }

    /// Forget every author that is no longer a member (a revocation that reached this host as a notice, or one that failed).
    pub async fn sweep(&self, is_member: &(dyn Fn(&str) -> bool + Sync)) -> usize {
        let gone: BTreeSet<String> = self
            .ledger
            .lock_recover()
            .values()
            .filter(|e| !is_member(&e.author))
            .map(|e| e.author.clone())
            .collect();
        let mut n = 0;
        for a in gone {
            n += self.forget_author(&a).await;
        }
        n
    }

    /// The host's review: accept (index it), or quarantine (take it out of the index, keep the record).
    ///
    /// # Errors
    ///
    /// Returns an error if the entry is unknown or the index would not take the change.
    pub async fn set_status(&self, doc: &str, status: Status) -> Result<()> {
        let _io = self.io.lock().await;
        let mut e = self
            .ledger
            .lock_recover()
            .get(doc)
            .cloned()
            .ok_or_else(|| anyhow!("no such entry"))?;
        if e.status == status {
            return Ok(());
        }
        e.status = status;
        match status {
            Status::Accepted => {
                self.cfg
                    .backend
                    .retain(&self.cfg.room, &self.item(&e))
                    .await?
            }
            Status::Pending | Status::Quarantined => {
                self.cfg.backend.delete(&self.cfg.room, doc).await?
            }
        }
        self.append(&Line::Status {
            doc: doc.to_string(),
            status,
        })?;
        self.ledger.lock_recover().insert(doc.to_string(), e);
        Ok(())
    }

    /// The entries waiting for review.
    #[must_use]
    pub fn pending(&self) -> Vec<Entry> {
        self.ledger
            .lock_recover()
            .values()
            .filter(|e| e.status == Status::Pending)
            .cloned()
            .collect()
    }

    /// Every entry the ledger holds, for the host's operator.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        self.ledger.lock_recover().values().cloned().collect()
    }

    /// Put every accepted entry back in the index: after the memory server lost its data, or was replaced.
    ///
    /// # Errors
    ///
    /// Returns an error at the first entry the index refuses.
    pub async fn reindex(&self) -> Result<usize> {
        let _io = self.io.lock().await;
        let accepted: Vec<Entry> = self
            .ledger
            .lock_recover()
            .values()
            .filter(|e| e.status == Status::Accepted)
            .cloned()
            .collect();
        for e in &accepted {
            self.cfg
                .backend
                .retain(&self.cfg.room, &self.item(e))
                .await?;
        }
        Ok(accepted.len())
    }
}
