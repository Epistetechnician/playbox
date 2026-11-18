//! Room-aware tray model and loopback watcher (`/api/rooms`).
//!
//! Signs in as Playbox Desktop over `127.0.0.1:<port>`, polls `/api/rooms` gently, and computes:
//! - the tray status summary line (`Hub running · 3 rooms · 1 needs you`),
//! - the room and attention lists for the menu-bar / system tray menu,
//! - the Dock / menu-bar badge count, and
//! - newly arrived attention notifications (seeded silently on the first poll so launching the app never replays old alerts).

use crate::signin;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomSummary {
    pub id: String,
    pub label: String,
    pub state: String,
    pub unread: usize,
    pub holds: usize,
}

impl RoomSummary {
    pub fn menu_label(&self) -> String {
        let dot = match self.state.as_str() {
            "live" => "●",
            "hold" | "bad" | "down" => "▲",
            _ => "○",
        };
        if self.holds > 0 {
            format!("{dot} {} (hold)", self.label)
        } else if self.unread > 0 {
            let n = if self.unread > 99 {
                "99+".to_string()
            } else {
                self.unread.to_string()
            };
            format!("{dot} {} ({n})", self.label)
        } else {
            format!("{dot} {}", self.label)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionItem {
    pub id: String,
    pub title: String,
    pub body: String,
    pub room: Option<String>,
    pub urgent: bool,
}

impl AttentionItem {
    pub fn menu_label(&self) -> String {
        let mark = if self.urgent { "●" } else { "•" };
        let clipped: String = self.title.chars().take(64).collect();
        format!("{mark} {clipped}")
    }

    pub fn target_path(&self) -> String {
        match &self.room {
            Some(r) if !r.is_empty() && valid_room_id(r) => format!("/r/{r}/"),
            _ => "/?view=attention".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HubSnapshot {
    pub rooms: Vec<RoomSummary>,
    pub attention: Vec<AttentionItem>,
}

impl HubSnapshot {
    pub fn badge_count(&self) -> usize {
        self.attention.len()
    }

    pub fn format_status(&self, base_line: &str) -> String {
        if self.rooms.is_empty() && self.attention.is_empty() {
            return base_line.to_string();
        }
        let room_part = if self.rooms.len() == 1 {
            "1 room".to_string()
        } else {
            format!("{} rooms", self.rooms.len())
        };
        if self.attention.is_empty() {
            let live = self.rooms.iter().filter(|r| r.state == "live").count();
            if live > 0 {
                format!("{base_line} · {room_part} ({live} live)")
            } else {
                format!("{base_line} · {room_part}")
            }
        } else {
            format!(
                "{base_line} · {room_part} · {} needs you",
                self.attention.len()
            )
        }
    }
}

fn valid_room_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Parse the JSON body of `GET /api/rooms` into a bounded `HubSnapshot`.
pub fn parse_rooms_json(body: &str) -> Option<HubSnapshot> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let rooms_arr = v.get("rooms")?.as_array()?;
    let mut rooms = Vec::new();
    for r in rooms_arr.iter().take(10) {
        let Some(id) = r.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        if !valid_room_id(id) {
            continue;
        }
        let label = r
            .get("label")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(id)
            .chars()
            .take(48)
            .collect::<String>();
        let state = r
            .get("state")
            .and_then(|x| x.as_str())
            .unwrap_or("idle")
            .to_string();
        let unread = r.get("unread").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let holds = r
            .get("holds")
            .and_then(|x| x.as_array())
            .map_or(0, Vec::len);
        rooms.push(RoomSummary {
            id: id.to_string(),
            label,
            state,
            unread,
            holds,
        });
    }

    let mut attention = Vec::new();
    if let Some(att_arr) = v.get("attention").and_then(|x| x.as_array()) {
        for a in att_arr.iter().take(20) {
            let Some(id) = a.get("id").and_then(|x| x.as_str()) else {
                continue;
            };
            let title = a
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("Needs your attention")
                .chars()
                .take(100)
                .collect::<String>();
            let body_txt = a
                .get("body")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .chars()
                .take(180)
                .collect::<String>();
            let room = a
                .get("room")
                .and_then(|x| x.as_str())
                .filter(|s| valid_room_id(s))
                .map(ToString::to_string);
            let urgent = a.get("urgent").and_then(|x| x.as_bool()).unwrap_or(false);
            attention.push(AttentionItem {
                id: id.to_string(),
                title,
                body: body_txt,
                room,
                urgent,
            });
        }
    }
    // Urgent items always lead in the menu and notifications.
    attention.sort_by_key(|a| !a.urgent);
    Some(HubSnapshot { rooms, attention })
}

/// Tracks seen attention items so notifications fire once per new event and never replay old items at launch.
#[derive(Default)]
pub struct DiffTracker {
    seeded: bool,
    seen: HashSet<String>,
}

impl DiffTracker {
    pub fn update(&mut self, snap: &HubSnapshot, window_focused: bool) -> Vec<AttentionItem> {
        let current_ids: HashSet<String> = snap.attention.iter().map(|a| a.id.clone()).collect();
        if !self.seeded {
            self.seeded = true;
            self.seen = current_ids;
            return Vec::new();
        }
        let mut notify = Vec::new();
        for item in &snap.attention {
            if !self.seen.contains(&item.id) && (item.urgent || !window_focused) {
                notify.push(item.clone());
            }
        }
        self.seen = current_ids;
        notify
    }

    pub fn reset(&mut self) {
        self.seeded = false;
        self.seen.clear();
    }
}

/// Minimal loopback client that holds its own `local_only` session cookie for reading `/api/rooms`.
#[derive(Default)]
pub struct LoopbackClient {
    cookie: Option<String>,
}

impl LoopbackClient {
    pub fn fetch_snapshot(&mut self, port: u16, hub_home: &Path) -> Option<HubSnapshot> {
        if self.cookie.is_none() {
            self.cookie = obtain_cookie(port, hub_home);
        }
        let cookie = self.cookie.as_deref()?;
        match get_with_cookie(port, "/api/rooms", cookie) {
            Some((200, body)) => parse_rooms_json(&body),
            Some((401 | 403, _)) => {
                self.cookie = obtain_cookie(port, hub_home);
                let fresh = self.cookie.as_deref()?;
                let (st, body) = get_with_cookie(port, "/api/rooms", fresh)?;
                (st == 200).then(|| parse_rooms_json(&body)).flatten()
            }
            _ => None,
        }
    }
}

fn obtain_cookie(port: u16, hub_home: &Path) -> Option<String> {
    if !signin::supported(port) {
        return None;
    }
    let nonce = signin::prepare(hub_home).ok()?;
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let limit = Duration::from_secs(2);
    let mut s = TcpStream::connect_timeout(&addr, limit).ok()?;
    let _ = s.set_read_timeout(Some(limit));
    let _ = s.set_write_timeout(Some(limit));
    let req = format!(
        "GET /desktop/signin?n={nonce} HTTP/1.1\r\nHost: localhost:{port}\r\nUser-Agent: PlayboxDesktop-Tray\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = String::new();
    let _ = s.take(32 * 1024).read_to_string(&mut raw);
    for line in raw.lines() {
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("set-cookie:") {
            let val = line["set-cookie:".len()..].trim();
            if let Some(pair) = val.split(';').next() {
                if pair.starts_with("pbs=") && pair.len() > 8 {
                    return Some(pair.to_string());
                }
            }
        }
    }
    None
}

fn get_with_cookie(port: u16, path: &str, cookie: &str) -> Option<(u16, String)> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let limit = Duration::from_secs(3);
    let mut s = TcpStream::connect_timeout(&addr, limit).ok()?;
    let _ = s.set_read_timeout(Some(limit));
    let _ = s.set_write_timeout(Some(limit));
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    let _ = s.take(256 * 1024).read_to_end(&mut raw);
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..sep]).ok()?;
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    let body_bytes = &raw[sep + 4..];
    let is_chunked = head.lines().any(|l| {
        let lower = l.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let decoded = if is_chunked {
        decode_chunked(body_bytes)?
    } else {
        body_bytes.to_vec()
    };
    Some((status, String::from_utf8_lossy(&decoded).into_owned()))
}

fn decode_chunked(mut input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let crlf = input.windows(2).position(|w| w == b"\r\n")?;
        let len_str = std::str::from_utf8(&input[..crlf])
            .ok()?
            .split(';')
            .next()?
            .trim();
        let len = usize::from_str_radix(len_str, 16).ok()?;
        input = &input[crlf + 2..];
        if len == 0 {
            break;
        }
        if input.len() < len + 2 {
            return None;
        }
        out.extend_from_slice(&input[..len]);
        input = &input[len + 2..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rooms_and_sorts_urgent_attention_first() {
        let json = r#"{
            "rooms": [
                {"id": "lab-1", "label": "GPU Lab", "state": "live", "unread": 3, "holds": []},
                {"id": "notes_2", "label": "Notes", "state": "idle", "unread": 0, "holds": [{"tool": "write"}]},
                {"id": "../bad", "label": "Skip", "state": "live", "unread": 0}
            ],
            "attention": [
                {"id": "unread:lab-1", "room": "lab-1", "title": "3 new in GPU Lab", "urgent": false},
                {"id": "hold:notes_2", "room": "notes_2", "title": "Witness paused write", "body": "write_file", "urgent": true}
            ]
        }"#;
        let snap = parse_rooms_json(json).unwrap();
        assert_eq!(snap.rooms.len(), 2);
        assert_eq!(snap.rooms[0].menu_label(), "● GPU Lab (3)");
        assert_eq!(snap.rooms[1].menu_label(), "○ Notes (hold)");
        assert_eq!(snap.badge_count(), 2);
        assert!(snap.attention[0].urgent);
        assert_eq!(snap.attention[0].target_path(), "/r/notes_2/");
        assert_eq!(
            snap.format_status("Hub running"),
            "Hub running · 2 rooms · 2 needs you"
        );
    }

    #[test]
    fn diff_tracker_seeds_silently_then_emits_urgent_and_unfocused_items_once() {
        let mut tracker = DiffTracker::default();
        let initial = HubSnapshot {
            rooms: vec![],
            attention: vec![AttentionItem {
                id: "old:1".into(),
                title: "Existing".into(),
                body: String::new(),
                room: Some("r1".into()),
                urgent: true,
            }],
        };
        assert!(tracker.update(&initial, false).is_empty());

        let next = HubSnapshot {
            rooms: vec![],
            attention: vec![
                initial.attention[0].clone(),
                AttentionItem {
                    id: "urgent:2".into(),
                    title: "Join request".into(),
                    body: "482 913".into(),
                    room: Some("r1".into()),
                    urgent: true,
                },
                AttentionItem {
                    id: "quiet:3".into(),
                    title: "1 unread".into(),
                    body: String::new(),
                    room: Some("r2".into()),
                    urgent: false,
                },
            ],
        };
        // Focused window: only urgent items buzz.
        let fired = tracker.update(&next, true);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].id, "urgent:2");

        // Repeating the same snapshot does not buzz again.
        assert!(tracker.update(&next, false).is_empty());
    }
}
