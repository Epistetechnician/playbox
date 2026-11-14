//! The room's live feed: what members say, and what happens in the room.
//!
//! One ordered log with a sequence number per item. Clients poll with the last number they saw, so a
//! person in a terminal and an agent behind any model or harness read the same thing the same way.
//! Items are not signed; the audit log is the record. A host with a state directory keeps the feed in an append-only file
//! (`chat.jsonl`) so a restart does not lose what was said, and a `say` that carries a request id is posted once however
//! many times it is sent. Text in `say` items comes from other members and must be treated as untrusted by agents.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const KEPT: usize = 1000;
const MAX_TEXT: usize = 2000;
const MAX_NAME: usize = 32;
const MAX_SEEN: usize = 1024;
/// Posts allowed per sender in one window.
const POSTS_PER_WINDOW: usize = 20;
const WINDOW_MS: u64 = 10_000;
/// A member that polled within this long counts as present.
pub const PRESENT_MS: u64 = 8_000;
/// How long a request id is remembered, and how many are, so a retried `say` is recognised.
pub const REQUEST_TTL_MS: u64 = 3_600_000;
const KEEP_REQUESTS: usize = 4096;
/// Longest request id a member may send.
pub const MAX_REQUEST_ID: usize = 64;
/// The file is rewritten (items and live request ids only) once it holds this many lines.
const COMPACT_AT: usize = KEPT * 4 + KEEP_REQUESTS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub seq: u64,
    pub time_ms: u64,
    /// `say` (a member or the host wrote it) or `event` (the room did something).
    pub kind: String,
    /// Full member id, or `host`, or empty for room-wide events.
    pub from: String,
    /// A label the sender chose. Not an identity: `from` is.
    pub name: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Presence {
    pub id: String,
    pub name: Option<String>,
    pub present: bool,
    pub last_seen_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Feed {
    pub items: Vec<Item>,
    /// Pass this as `since` on the next poll.
    pub next: u64,
    pub now_ms: u64,
    pub members: Vec<Presence>,
}

/// What a request id was used for: the item it produced and a hash of what was said, so the same id with other text is
/// refused instead of silently answered with the first message's number.
struct Request {
    seq: u64,
    hash: String,
    at_ms: u64,
}

struct Store {
    path: PathBuf,
    file: File,
    lines: usize,
}

#[derive(Default)]
struct Inner {
    next: u64,
    items: VecDeque<Item>,
    names: HashMap<String, String>,
    seen: HashMap<String, u64>,
    posts: HashMap<String, VecDeque<u64>>,
    /// (member, request id) to the post it made.
    requests: HashMap<(String, String), Request>,
    request_order: VecDeque<(String, String)>,
    store: Option<Store>,
}

#[derive(Default)]
pub struct Chat {
    inner: Mutex<Inner>,
    /// Woken whenever something is added to the feed, so a long-poll reader returns at once instead of on its next poll.
    notify: tokio::sync::Notify,
}

/// The longest a feed read may wait for something new. Below the connection's idle timeout, so a quiet room never looks like a dead peer.
pub const MAX_FEED_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Drop control characters (a terminal must not be steered by a member's text) and cap the length.
fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

impl Chat {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn push(inner: &mut Inner, kind: &str, from: &str, name: Option<String>, text: String) -> u64 {
        inner.next += 1;
        let seq = inner.next;
        inner.items.push_back(Item {
            seq,
            time_ms: now_ms(),
            kind: kind.into(),
            from: from.into(),
            name,
            text,
        });
        while inner.items.len() > KEPT {
            inner.items.pop_front();
        }
        if let Some(item) = inner.items.back().cloned() {
            Self::persist(inner, &json!({ "t": "item", "item": item }));
        }
        seq
    }

    /// Append one line to the store, and compact it when it has grown. A failure is reported on stderr and the feed carries
    /// on in memory, the way a failed audit write does.
    fn persist(inner: &mut Inner, line: &Value) {
        let Some(store) = inner.store.as_mut() else { return };
        let mut text = line.to_string();
        text.push('\n');
        if let Err(e) = store.file.write_all(text.as_bytes()) {
            eprintln!("playbox: cannot write the chat log {}: {e}", store.path.display());
            return;
        }
        store.lines += 1;
        if store.lines > COMPACT_AT {
            Self::compact(inner);
        }
    }

    fn compact(inner: &mut Inner) {
        let Some(store) = inner.store.as_ref() else { return };
        let path = store.path.clone();
        let mut body = String::new();
        let mut lines = 0;
        for item in &inner.items {
            body.push_str(&json!({ "t": "item", "item": item }).to_string());
            body.push('\n');
            lines += 1;
        }
        for key in &inner.request_order {
            if let Some(r) = inner.requests.get(key) {
                body.push_str(&Self::request_line(key, r).to_string());
                body.push('\n');
                lines += 1;
            }
        }
        let tmp = path.with_extension("tmp");
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| f.write_all(body.as_bytes()).and_then(|()| f.sync_all()))
            .and_then(|()| std::fs::rename(&tmp, &path))
            .and_then(|()| std::fs::OpenOptions::new().append(true).open(&path));
        match written {
            Ok(file) => inner.store = Some(Store { path, file, lines }),
            Err(e) => eprintln!("playbox: cannot compact the chat log {}: {e}", path.display()),
        }
    }

    fn request_line(key: &(String, String), r: &Request) -> Value {
        json!({ "t": "req", "from": key.0, "id": key.1, "seq": r.seq, "hash": r.hash, "at_ms": r.at_ms })
    }

    /// Open the feed kept in `path`: what was said and done survives a host restart, and so do request ids, so a `say`
    /// retried across the restart is still recognised. Lines that do not parse are skipped. If the file cannot be opened
    /// for appending the feed runs in memory only, with a note on stderr.
    #[must_use]
    pub fn open(path: &Path) -> Self {
        let chat = Self::new();
        {
            let mut g = chat.inner.lock().expect("chat lock");
            let mut lines = 0usize;
            // Bytes, not a string: a crash can cut the last line inside a multi-byte character, and that must cost
            // that one line, not the whole history.
            if let Ok(bytes) = std::fs::read(path) {
                if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                    // End the torn line, or the next entry would be glued to it and lost with it.
                    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
                        let _ = std::io::Write::write_all(&mut f, b"\n");
                    }
                }
                let data = String::from_utf8_lossy(&bytes);
                let now = now_ms();
                for l in data.lines() {
                    lines += 1;
                    let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
                    match v["t"].as_str() {
                        Some("item") => {
                            if let Ok(item) = serde_json::from_value::<Item>(v["item"].clone()) {
                                g.next = g.next.max(item.seq);
                                if let (true, Some(n)) = (item.kind == "say", &item.name) {
                                    g.names.insert(item.from.clone(), n.clone());
                                }
                                g.items.push_back(item);
                                while g.items.len() > KEPT {
                                    g.items.pop_front();
                                }
                            }
                        }
                        Some("req") => {
                            let (Some(from), Some(id), Some(seq), Some(hash), Some(at_ms)) = (
                                v["from"].as_str(),
                                v["id"].as_str(),
                                v["seq"].as_u64(),
                                v["hash"].as_str(),
                                v["at_ms"].as_u64(),
                            ) else {
                                continue;
                            };
                            if now.saturating_sub(at_ms) <= REQUEST_TTL_MS {
                                let key = (from.to_string(), id.to_string());
                                if g.requests.insert(key.clone(), Request { seq, hash: hash.to_string(), at_ms }).is_none() {
                                    g.request_order.push_back(key);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            match File::options().create(true).append(true).mode(0o600).open(path) {
                Ok(file) => {
                    g.store = Some(Store { path: path.to_path_buf(), file, lines });
                    if lines > COMPACT_AT {
                        Self::compact(&mut g);
                    }
                }
                Err(e) => eprintln!("playbox: cannot keep the chat log {}: {e}", path.display()),
            }
        }
        chat
    }

    /// A member (or `host`) says something. Refused when empty or when the sender is posting too fast.
    ///
    /// # Errors
    ///
    /// Returns a message describing why the operation failed.
    pub fn say(&self, from: &str, name: Option<&str>, text: &str) -> Result<u64, String> {
        self.say_once(from, name, text, None)
    }

    /// [`Chat::say`] that posts at most once per `request_id`. A second `say` from the same member with the same id and the
    /// same text returns the first one's number and posts nothing (it does not count against the rate limit either); the
    /// same id with other text is refused. This is what lets a sender retry after a lost reply without saying it twice.
    ///
    /// # Errors
    ///
    /// Returns a message describing why the operation failed.
    pub fn say_once(&self, from: &str, name: Option<&str>, text: &str, request_id: Option<&str>) -> Result<u64, String> {
        if let Some(id) = request_id {
            if id.is_empty() || id.len() > MAX_REQUEST_ID || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) {
                return Err(format!("a request id is 1 to {MAX_REQUEST_ID} letters, digits, - _ or ."));
            }
        }
        let text = clean(text, MAX_TEXT);
        if text.is_empty() {
            return Err("nothing to say".into());
        }
        let name = name.map(|n| clean(n, MAX_NAME)).filter(|n| !n.is_empty());
        let mut g = self.inner.lock().expect("chat lock");
        let now = now_ms();
        let key = request_id.map(|id| (from.to_string(), id.to_string()));
        let hash = blake3::hash(format!("{}\n{text}", name.as_deref().unwrap_or("")).as_bytes()).to_hex()[..32].to_string();
        if let Some(key) = &key {
            while g.request_order.front().is_some_and(|k| g.requests.get(k).is_none_or(|r| now.saturating_sub(r.at_ms) > REQUEST_TTL_MS)) {
                if let Some(old) = g.request_order.pop_front() {
                    g.requests.remove(&old);
                }
            }
            if let Some(r) = g.requests.get(key) {
                return if r.hash == hash {
                    Ok(r.seq)
                } else {
                    Err("that request id was already used for a different message".into())
                };
            }
        }
        let q = g.posts.entry(from.to_string()).or_default();
        while q
            .front()
            .is_some_and(|t| now.saturating_sub(*t) > WINDOW_MS)
        {
            q.pop_front();
        }
        if q.len() >= POSTS_PER_WINDOW {
            return Err(format!(
                "slow down: at most {POSTS_PER_WINDOW} messages per {} s",
                WINDOW_MS / 1000
            ));
        }
        q.push_back(now);
        if let Some(n) = &name {
            g.names.insert(from.to_string(), n.clone());
        }
        Self::mark_seen(&mut g, from, now);
        let name = name.or_else(|| g.names.get(from).cloned());
        let seq = Self::push(&mut g, "say", from, name, text);
        if let Some(key) = key {
            let r = Request { seq, hash, at_ms: now };
            let line = Self::request_line(&key, &r);
            g.request_order.push_back(key.clone());
            g.requests.insert(key, r);
            while g.request_order.len() > KEEP_REQUESTS {
                if let Some(old) = g.request_order.pop_front() {
                    g.requests.remove(&old);
                }
            }
            Self::persist(&mut g, &line);
        }
        drop(g);
        self.notify.notify_waiters();
        Ok(seq)
    }

    /// Something happened in the room. Only a short summary is kept, never arguments or reasons in full.
    pub fn event(&self, event: &str, peer: Option<&str>, details: &Value) {
        let mut g = self.inner.lock().expect("chat lock");
        if let Some(p) = peer {
            Self::mark_seen(&mut g, p, now_ms());
        }
        let text = summarize(event, details);
        Self::push(&mut g, "event", peer.unwrap_or(""), None, text);
        drop(g);
        self.notify.notify_waiters();
    }

    /// Mark a member as present (they polled).
    pub fn touch(&self, who: &str) {
        let mut g = self.inner.lock().expect("chat lock");
        Self::mark_seen(&mut g, who, now_ms());
    }

    /// Record when `who` was last here. Audit events name strangers too (a refused join, a denied request) and a stranger
    /// can use any number of keys, so the table is capped: when it is full the longest-silent entry makes room.
    fn mark_seen(g: &mut Inner, who: &str, now: u64) {
        if g.seen.len() >= MAX_SEEN && !g.seen.contains_key(who) {
            if let Some(oldest) = g.seen.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone()) {
                g.seen.remove(&oldest);
            }
        }
        g.seen.insert(who.to_string(), now);
    }

    /// [`Chat::since`], but when nothing is new yet wait up to `wait` (at most [`MAX_FEED_WAIT`]) for something to arrive and return as
    /// soon as it does. `who` is marked present while waiting, so a patient reader does not look gone. A zero `wait` is a plain read.
    pub async fn wait_since(&self, who: &str, since: u64, limit: usize, members: &[String], wait: std::time::Duration) -> Feed {
        let deadline = std::time::Instant::now() + wait.min(MAX_FEED_WAIT);
        loop {
            // Register for wake-ups before looking, so a message that lands in between is not missed.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            self.touch(who);
            let feed = self.since(since, limit, members);
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if !feed.items.is_empty() || left.is_zero() {
                return feed;
            }
            // Wake now and then even without news, to keep `who` present across a long wait.
            let _ = tokio::time::timeout(left.min(std::time::Duration::from_secs(4)), notified).await;
        }
    }

    /// Everything after `since` (at most `limit`), plus who is here. `members` are the ids to list.
    pub fn since(&self, since: u64, limit: usize, members: &[String]) -> Feed {
        let g = self.inner.lock().expect("chat lock");
        let now = now_ms();
        let items: Vec<Item> = g
            .items
            .iter()
            .filter(|i| i.seq > since)
            .take(limit.clamp(1, 500))
            .cloned()
            .collect();
        // A cursor ahead of the log (the host restarted) is pulled back so new items are not missed.
        let next = items.last().map_or(since.min(g.next), |i| i.seq);
        let members = members
            .iter()
            .map(|id| {
                let last = g.seen.get(id).copied().unwrap_or(0);
                Presence {
                    id: id.clone(),
                    name: g.names.get(id).cloned(),
                    present: last > 0 && now.saturating_sub(last) < PRESENT_MS,
                    last_seen_ms: last,
                }
            })
            .collect();
        Feed {
            items,
            next,
            now_ms: now,
            members,
        }
    }
}

fn clip(s: &str, n: usize) -> String {
    let c: String = s.chars().filter(|c| !c.is_control()).take(n).collect();
    c
}

fn field<'a>(d: &'a Value, k: &str) -> Option<&'a Value> {
    d.get(k).filter(|v| !v.is_null())
}

/// One line for the feed from an audit event. Only fields that are safe for every member to see.
pub fn summarize(event: &str, d: &Value) -> String {
    let ok = field(d, "ok").and_then(Value::as_bool);
    let verdict = match ok {
        Some(true) => "ok",
        Some(false) => "failed",
        None => "",
    };
    let s = |k: &str| field(d, k).and_then(Value::as_str).map(|v| clip(v, 60));
    let mut parts: Vec<String> = vec![event.to_string()];
    if !verdict.is_empty() {
        parts.push(verdict.into());
    }
    match event {
        "pull" => parts.extend(s("ref")),
        "push" => parts.extend(s("branch")),
        "task" => {
            parts.extend(s("program"));
            if let Some(c) = field(d, "exit_code").and_then(Value::as_i64) {
                parts.push(format!("exit {c}"));
            }
            if field(d, "timed_out").and_then(Value::as_bool) == Some(true) {
                parts.push("timed out".into());
            }
        }
        "steer" => {
            parts.extend(s("kind"));
            parts.extend(s("model"));
        }
        "refused" => parts.extend(s("reason").map(|r| clip(&r, 60))),
        _ => {}
    }
    parts.join(" ")
}

impl Feed {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_ordered_and_polled_by_sequence() {
        let c = Chat::new();
        let a = c.say("alice", Some("Alice"), "hello").unwrap();
        let b = c.say("bob", None, "hi alice").unwrap();
        assert!(b > a);
        let f = c.since(0, 100, &[]);
        assert_eq!(f.items.len(), 2);
        assert_eq!(f.items[0].name.as_deref(), Some("Alice"));
        assert_eq!(f.next, b);
        let none = c.since(f.next, 100, &[]);
        assert!(none.items.is_empty());
        assert_eq!(none.next, b);
    }

    #[test]
    fn control_characters_are_stripped_and_text_is_capped() {
        let c = Chat::new();
        c.say("m", Some("n\u{1b}[31mx"), "a\u{1b}[2Jb\u{7}")
            .unwrap();
        let f = c.since(0, 10, &[]);
        assert!(!f.items[0].text.contains('\u{1b}'));
        assert!(!f.items[0].name.as_deref().unwrap().contains('\u{1b}'));
        c.say("m", None, &"x".repeat(MAX_TEXT * 2)).unwrap();
        assert_eq!(c.since(0, 10, &[]).items[1].text.chars().count(), MAX_TEXT);
    }

    #[test]
    fn empty_messages_and_floods_are_refused() {
        let c = Chat::new();
        assert!(c.say("m", None, "   ").is_err());
        for _ in 0..POSTS_PER_WINDOW {
            c.say("m", None, "x").unwrap();
        }
        assert!(c.say("m", None, "x").is_err());
        assert!(c.say("other", None, "x").is_ok());
    }

    #[test]
    fn strangers_cannot_grow_the_presence_table_without_limit() {
        let c = Chat::new();
        for i in 0..MAX_SEEN * 3 {
            c.event("join", Some(&format!("stranger{i}")), &json!({"ok": false}));
        }
        assert!(c.inner.lock().unwrap().seen.len() <= MAX_SEEN);
    }

    #[test]
    fn a_log_cut_inside_a_multibyte_character_keeps_the_history() {
        let p = std::env::temp_dir().join(format!("playbox-chat-torn-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let c = Chat::open(&p);
        c.say("m", Some("n"), "before").unwrap();
        drop(c);
        // A crash after the first byte of a two-byte character in the next line.
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, b"{\"t\":\"item\",\"item\":{\"text\":\"\xc3").unwrap();
        drop(f);
        let c = Chat::open(&p);
        let items = c.since(0, 10, &[]).items;
        assert_eq!(items.len(), 1, "the history before the torn line survives");
        assert_eq!(items[0].text, "before");
        c.say("m", Some("n"), "after").unwrap();
        drop(c);
        let texts: Vec<String> = Chat::open(&p).since(0, 10, &[]).items.into_iter().map(|i| i.text).collect();
        assert_eq!(texts, ["before", "after"], "a line written after the torn one is not glued to it");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn events_keep_only_a_short_summary() {
        let c = Chat::new();
        c.event(
            "task",
            Some("p"),
            &json!({"ok": true, "program": "sh", "exit_code": 0, "cmd_hash": "secret", "args": 2}),
        );
        let t = &c.since(0, 10, &[]).items[0].text;
        assert_eq!(t, "task ok sh exit 0");
    }

    #[test]
    fn presence_follows_polling() {
        let c = Chat::new();
        c.touch("alice");
        let f = c.since(0, 10, &["alice".into(), "bob".into()]);
        assert!(f.members[0].present);
        assert!(!f.members[1].present);
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("playbox-chat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("chat.jsonl")
    }

    #[test]
    fn a_request_id_posts_once_and_the_same_id_with_other_text_is_refused() {
        let c = Chat::new();
        let first = c.say_once("alice", None, "hello", Some("r1")).unwrap();
        assert_eq!(c.say_once("alice", None, "hello", Some("r1")).unwrap(), first);
        assert_eq!(c.since(0, 100, &[]).items.iter().filter(|i| i.kind == "say").count(), 1);
        assert!(c.say_once("alice", None, "something else", Some("r1")).unwrap_err().contains("different message"));
        // Another member may use the same id, and a message without an id is never deduplicated.
        let other = c.say_once("bob", None, "hello", Some("r1")).unwrap();
        assert_ne!(other, first);
        let a = c.say_once("alice", None, "hello", None).unwrap();
        let b = c.say_once("alice", None, "hello", None).unwrap();
        assert_ne!(a, b);
        for bad in ["", "has space", "new\nline", &"x".repeat(MAX_REQUEST_ID + 1), "é"] {
            assert!(c.say_once("alice", None, "hi", Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_retry_does_not_use_up_the_rate_limit() {
        let c = Chat::new();
        for i in 0..POSTS_PER_WINDOW {
            c.say_once("alice", None, &format!("m{i}"), Some(&format!("r{i}"))).unwrap();
        }
        assert!(c.say_once("alice", None, "over", Some("new")).is_err(), "the window is full");
        for i in 0..POSTS_PER_WINDOW {
            assert!(c.say_once("alice", None, &format!("m{i}"), Some(&format!("r{i}"))).is_ok(), "a retry of r{i} is answered");
        }
    }

    #[test]
    fn the_feed_and_request_ids_survive_a_restart() {
        let path = temp("restart");
        let c = Chat::open(&path);
        let seq = c.say_once("alice", Some("Alice"), "hello", Some("r1")).unwrap();
        c.event("join", Some("bob"), &json!({}));
        drop(c);
        let c = Chat::open(&path);
        let f = c.since(0, 100, &[]);
        assert_eq!(f.items.len(), 2);
        assert_eq!(f.items[0].text, "hello");
        assert_eq!(f.items[0].name.as_deref(), Some("Alice"));
        // The retry after the restart is recognised, and new posts continue the numbering.
        assert_eq!(c.say_once("alice", Some("Alice"), "hello", Some("r1")).unwrap(), seq);
        let next = c.say_once("alice", None, "again", Some("r2")).unwrap();
        assert!(next > f.items[1].seq);
        assert_eq!(c.since(0, 100, &[]).items.iter().filter(|i| i.kind == "say").count(), 2);
        // A display name set earlier is remembered for later posts without one.
        c.say("alice", None, "no name given").unwrap();
        assert_eq!(c.since(0, 100, &[]).items.last().unwrap().name.as_deref(), Some("Alice"));
    }

    #[test]
    fn a_damaged_log_is_read_as_far_as_it_goes_and_the_chat_keeps_working() {
        let path = temp("damaged");
        let c = Chat::open(&path);
        c.say("alice", None, "kept").unwrap();
        drop(c);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not json\n{\"t\":\"item\",\"item\":5}\n{\"t\":\"req\"}\n\u{0}\n");
        std::fs::write(&path, text).unwrap();
        let c = Chat::open(&path);
        assert_eq!(c.since(0, 100, &[]).items.len(), 1);
        assert!(c.say("alice", None, "still works").is_ok());
        // A path that cannot be opened gives an in-memory chat, not a panic.
        let c = Chat::open(std::path::Path::new("/nonexistent-dir/chat.jsonl"));
        assert!(c.say("alice", None, "memory only").is_ok());
    }

    #[test]
    fn the_log_is_compacted_and_keeps_only_what_the_feed_keeps() {
        let path = temp("compact");
        let c = Chat::open(&path);
        let n = COMPACT_AT + 50;
        for i in 0..n {
            // The rate limit is per window, so post as many different members as needed.
            c.say_once(&format!("m{}", i % 1000), None, &format!("message {i}"), Some(&format!("r{i}"))).unwrap();
        }
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert!(lines < COMPACT_AT + 60, "{lines} lines");
        drop(c);
        let c = Chat::open(&path);
        let (mut all, mut cursor) = (Vec::new(), 0);
        loop {
            let f = c.since(cursor, 500, &[]);
            if f.items.is_empty() {
                break;
            }
            cursor = f.next;
            all.extend(f.items);
        }
        assert_eq!(all.len(), KEPT);
        let last = all.last().unwrap();
        assert!(last.text.ends_with(&format!("message {}", n - 1)), "{}", last.text);
        // A recent id still deduplicates after compaction and reload.
        let who = format!("m{}", (n - 1) % 1000);
        assert_eq!(c.say_once(&who, None, &format!("message {}", n - 1), Some(&format!("r{}", n - 1))).unwrap(), last.seq);
    }
    #[tokio::test]
    async fn a_waiting_read_returns_when_something_arrives() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let c = Arc::new(Chat::new());
        let ids = vec!["host".to_string(), "m1".to_string()];
        // Something already there: no waiting.
        c.say("m1", None, "first").unwrap();
        let t = Instant::now();
        let f = c.wait_since("m2", 0, 50, &ids, Duration::from_secs(5)).await;
        assert_eq!(f.items.len(), 1);
        assert!(t.elapsed() < Duration::from_millis(500), "an available item must not wait");
        // Nothing new: it waits for the next say and returns at once when it lands.
        let c2 = Arc::clone(&c);
        let ids2 = ids.clone();
        let t = Instant::now();
        let waiter = tokio::spawn(async move { c2.wait_since("m2", 1, 50, &ids2, Duration::from_secs(10)).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        c.say("m1", None, "second").unwrap();
        let f = waiter.await.unwrap();
        assert_eq!(f.items.len(), 1);
        assert_eq!(f.items[0].text, "second");
        assert!(t.elapsed() >= Duration::from_millis(250) && t.elapsed() < Duration::from_secs(3), "returned on the message, not on the timeout: {:?}", t.elapsed());
        // An event wakes a waiter too (a join, a push).
        let c3 = Arc::clone(&c);
        let waiter = tokio::spawn(async move { c3.wait_since("m2", 2, 50, &["host".to_string()], Duration::from_secs(10)).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        c.event("join_ok", Some("m3"), &serde_json::json!({}));
        assert_eq!(waiter.await.unwrap().items.len(), 1);
    }

    #[tokio::test]
    async fn a_waiting_read_gives_up_empty_and_is_capped() {
        use std::time::{Duration, Instant};
        let c = Chat::new();
        let ids = vec!["host".to_string()];
        let t = Instant::now();
        let f = c.wait_since("m1", 0, 50, &ids, Duration::from_millis(400)).await;
        assert!(f.items.is_empty());
        assert!(t.elapsed() >= Duration::from_millis(350) && t.elapsed() < Duration::from_secs(2));
        // Zero is a plain read.
        let t = Instant::now();
        assert!(c.wait_since("m1", 0, 50, &ids, Duration::ZERO).await.items.is_empty());
        assert!(t.elapsed() < Duration::from_millis(200));
        // The reader counts as present while it waits.
        assert!(c.since(0, 10, &["m1".to_string()]).members[0].present);
        assert_eq!(MAX_FEED_WAIT, Duration::from_secs(20));
    }
}
