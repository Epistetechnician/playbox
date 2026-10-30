//! The room's live feed: what members say, and what happens in the room.
//!
//! One ordered log with a sequence number per item. Clients poll with the last number they saw, so a
//! person in a terminal and an agent behind any model or harness read the same thing the same way.
//! Items are not signed and not kept across restarts; the audit log is the record. Text in `say` items
//! comes from other members and must be treated as untrusted by agents.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const KEPT: usize = 1000;
const MAX_TEXT: usize = 2000;
const MAX_NAME: usize = 32;
/// Posts allowed per sender in one window.
const POSTS_PER_WINDOW: usize = 20;
const WINDOW_MS: u64 = 10_000;
/// A member that polled within this long counts as present.
pub const PRESENT_MS: u64 = 8_000;

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

#[derive(Default)]
struct Inner {
    next: u64,
    items: VecDeque<Item>,
    names: HashMap<String, String>,
    seen: HashMap<String, u64>,
    posts: HashMap<String, VecDeque<u64>>,
}

#[derive(Default)]
pub struct Chat {
    inner: Mutex<Inner>,
}

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
        seq
    }

    /// A member (or `host`) says something. Refused when empty or when the sender is posting too fast.
    pub fn say(&self, from: &str, name: Option<&str>, text: &str) -> Result<u64, String> {
        let text = clean(text, MAX_TEXT);
        if text.is_empty() {
            return Err("nothing to say".into());
        }
        let name = name.map(|n| clean(n, MAX_NAME)).filter(|n| !n.is_empty());
        let mut g = self.inner.lock().expect("chat lock");
        let now = now_ms();
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
        g.seen.insert(from.to_string(), now);
        let name = name.or_else(|| g.names.get(from).cloned());
        Ok(Self::push(&mut g, "say", from, name, text))
    }

    /// Something happened in the room. Only a short summary is kept, never arguments or reasons in full.
    pub fn event(&self, event: &str, peer: Option<&str>, details: &Value) {
        let mut g = self.inner.lock().expect("chat lock");
        if let Some(p) = peer {
            g.seen.insert(p.to_string(), now_ms());
        }
        let text = summarize(event, details);
        Self::push(&mut g, "event", peer.unwrap_or(""), None, text);
    }

    /// Mark a member as present (they polled).
    pub fn touch(&self, who: &str) {
        self.inner
            .lock()
            .expect("chat lock")
            .seen
            .insert(who.to_string(), now_ms());
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
}
