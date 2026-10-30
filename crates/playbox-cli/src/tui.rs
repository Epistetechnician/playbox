//! A chat room in the terminal: what members say, what happens in the room, who is here.
//!
//! This is only a viewer of the room's feed (`playbox_net::chat`). A person types here; an agent behind any
//! model or harness uses the `playbox_say` / `playbox_feed` MCP tools, and both appear in the same stream.
//! A background task polls the host about once a second, so the screen never waits on the network.

use anyhow::Result;
use playbox_net::{
    chat::{Feed, Item, Presence},
    Client, Host,
};
use ratatui::{
    crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

const POLL_EVERY: Duration = Duration::from_millis(1000);
const POLL_TIMEOUT: Duration = Duration::from_secs(8);
const KEEP_ITEMS: usize = 2000;
const SIDEBAR: u16 = 26;

/// Whose view this is.
#[derive(Clone)]
pub enum Backend {
    Guest(Arc<Client>),
    Host(Arc<Host>),
}

impl Backend {
    async fn feed(&self, since: u64) -> Result<Feed> {
        match self {
            Backend::Guest(c) => {
                let mut f = c.feed(since).await?;
                // The host answered, so it is here; the feed lists only members.
                f.members.insert(0, Presence { id: "host".into(), name: None, present: true, last_seen_ms: f.now_ms });
                Ok(f)
            }
            Backend::Host(h) => Ok(h.feed(since)),
        }
    }

    async fn say(&self, text: &str, name: Option<&str>) -> Result<()> {
        match self {
            Backend::Guest(c) => c.say(text, name).await.map(|_| ()),
            Backend::Host(h) => h.say(text, name).map(|_| ()).map_err(anyhow::Error::msg),
        }
    }

    /// The id this view posts as.
    fn me(&self) -> String {
        match self {
            Backend::Guest(c) => c.id().to_string(),
            Backend::Host(_) => "host".into(),
        }
    }
}

enum Event {
    Feed(Feed, Duration),
    Down(String),
    Note(String),
}

#[derive(Clone, PartialEq, Debug)]
pub enum Status {
    Connecting,
    Live(Duration),
    Down(String),
}

pub struct App {
    pub title: String,
    pub me: String,
    pub name: Option<String>,
    pub host_mode: bool,
    pub items: Vec<Item>,
    pub members: Vec<Presence>,
    pub input: String,
    /// Lines scrolled up from the newest.
    pub scroll: usize,
    pub status: Status,
    pub quit: bool,
}

impl App {
    pub fn new(title: String, me: String, name: Option<String>, host_mode: bool) -> Self {
        Self { title, me, name, host_mode, items: vec![], members: vec![], input: String::new(), scroll: 0, status: Status::Connecting, quit: false }
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.items.push(Item { seq: 0, time_ms: playbox_net::chat::now_ms(), kind: "local".into(), from: String::new(), name: None, text: text.into() });
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Feed(f, took) => {
                self.items.extend(f.items);
                if self.items.len() > KEEP_ITEMS {
                    self.items.drain(..self.items.len() - KEEP_ITEMS);
                }
                self.members = f.members;
                self.status = Status::Live(took);
            }
            Event::Down(e) => self.status = Status::Down(e),
            Event::Note(t) => self.note(t),
        }
    }
}

fn short(id: &str) -> String {
    id.chars().take(4).collect()
}

fn color_for(id: &str) -> Color {
    const PALETTE: [Color; 6] = [Color::Green, Color::Magenta, Color::Blue, Color::LightRed, Color::LightCyan, Color::LightYellow];
    PALETTE[id.bytes().fold(0usize, |a, b| a.wrapping_add(b as usize)) % PALETTE.len()]
}

/// "name·1a15", "1a15" or "host".
pub fn label(from: &str, name: Option<&str>) -> String {
    match (from, name) {
        ("host", _) => "host".into(),
        (f, Some(n)) => format!("{n}·{}", short(f)),
        (f, None) => short(f),
    }
}

fn clock(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Greedy word wrap that also breaks words longer than the line.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = vec![];
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            while word.chars().count() > width {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let head: String = word.chars().take(width).collect();
                word = word.chars().skip(width).collect();
                out.push(head);
            }
            let need = if line.is_empty() { word.chars().count() } else { line.chars().count() + 1 + word.chars().count() };
            if need > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
        out.push(line);
    }
    out
}

fn item_lines(i: &Item, me: &str, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let t = clock(i.time_ms);
    match i.kind.as_str() {
        "say" => {
            let who = label(&i.from, i.name.as_deref());
            let style = if i.from == me { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else if i.from == "host" { Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD) } else { Style::default().fg(color_for(&i.from)).add_modifier(Modifier::BOLD) };
            let prefix = t.chars().count() + 2 + who.chars().count() + 2;
            let body = wrap(&i.text, width.saturating_sub(prefix));
            body.into_iter()
                .enumerate()
                .map(|(n, l)| {
                    if n == 0 {
                        Line::from(vec![Span::styled(format!("{t}  "), dim), Span::styled(who.clone(), style), Span::raw(format!("  {l}"))])
                    } else {
                        Line::from(format!("{}{l}", " ".repeat(prefix)))
                    }
                })
                .collect()
        }
        "event" => {
            let peer = if i.from.is_empty() { String::new() } else { format!("  ({})", short(&i.from)) };
            wrap(&format!("• {}{peer}", i.text), width.saturating_sub(t.chars().count() + 2))
                .into_iter()
                .enumerate()
                .map(|(n, l)| if n == 0 { Line::from(Span::styled(format!("{t}  {l}"), dim)) } else { Line::from(Span::styled(format!("{}{l}", " ".repeat(t.chars().count() + 2)), dim)) })
                .collect()
        }
        _ => wrap(&i.text, width).into_iter().map(|l| Line::from(Span::styled(l, Style::default().fg(Color::Yellow)))).collect(),
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let [header, body, input] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3)]).areas(f.area());

    let (dot, status_style, status_text) = match &app.status {
        Status::Connecting => ("○", Style::default().fg(Color::Yellow), "connecting".to_string()),
        Status::Live(d) => ("●", Style::default().fg(Color::Green), format!("live {} ms", d.as_millis())),
        Status::Down(e) => ("○", Style::default().fg(Color::Red), format!("reconnecting: {}", e.chars().take(40).collect::<String>())),
    };
    let you = label(&app.me, app.name.as_deref());
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" playbox ", Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)),
            Span::raw(format!(" {}  ·  you: {you}  ·  ", app.title)),
            Span::styled(format!("{dot} {status_text}"), status_style),
        ])),
        header,
    );

    let (main, side): (Rect, Option<Rect>) = if body.width >= 60 {
        let [m, s] = Layout::horizontal([Constraint::Min(30), Constraint::Length(SIDEBAR)]).areas(body);
        (m, Some(s))
    } else {
        (body, None)
    };

    // Messages: wrap everything, show the newest lines that fit (minus the scroll offset).
    let inner_w = main.width.saturating_sub(2) as usize;
    let inner_h = main.height.saturating_sub(2) as usize;
    let all: Vec<Line> = app.items.iter().flat_map(|i| item_lines(i, &app.me, inner_w)).collect();
    let max_scroll = all.len().saturating_sub(inner_h);
    let scroll = app.scroll.min(max_scroll);
    let end = all.len() - scroll;
    let start = end.saturating_sub(inner_h);
    let title = if scroll > 0 { format!(" room · scrolled up {scroll} (End to follow) ") } else { " room ".to_string() };
    f.render_widget(Paragraph::new(all[start..end].to_vec()).block(Block::default().borders(Borders::ALL).title(title)), main);

    if let Some(side) = side {
        let present = app.members.iter().filter(|m| m.present).count();
        let lines: Vec<Line> = app
            .members
            .iter()
            .map(|m| {
                let who = if m.id == app.me { format!("{} (you)", label(&m.id, m.name.as_deref())) } else { label(&m.id, m.name.as_deref()) };
                if m.present {
                    Line::from(vec![Span::styled("● ", Style::default().fg(Color::Green)), Span::raw(who)])
                } else {
                    Line::from(Span::styled(format!("○ {who}"), Style::default().fg(Color::DarkGray)))
                }
            })
            .collect();
        f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(format!(" here {present}/{} ", app.members.len()))), side);
    }

    let hint = if app.host_mode { " say something · /link /revoke /members /help · Esc quits " } else { " say something · /name /help · Esc quits " };
    f.render_widget(Paragraph::new(format!("› {}", app.input)).block(Block::default().borders(Borders::ALL).title(hint)), input);
    let x = input.x + 3 + app.input.chars().count().min(input.width.saturating_sub(4) as usize) as u16;
    f.set_cursor_position((x, input.y + 1));
}

pub enum Action {
    None,
    Say(String),
    Link,
    Revoke(String),
}

/// Handle one line typed into the box.
pub fn submit(app: &mut App) -> Action {
    let line = app.input.trim().to_string();
    app.input.clear();
    app.scroll = 0;
    if line.is_empty() {
        return Action::None;
    }
    let Some(cmd) = line.strip_prefix('/') else { return Action::Say(line) };
    let mut w = cmd.splitn(2, ' ');
    match (w.next().unwrap_or(""), w.next().map(str::trim)) {
        ("quit" | "q", _) => app.quit = true,
        ("help" | "h", _) => {
            app.note("Type to talk to everyone. Agents of any model post here through playbox_say.");
            if app.host_mode {
                app.note("/link  new one-hour invite   /revoke ID  remove a member (id prefix)   /members  list ids   /quit");
            } else {
                app.note("/name LABEL  change the label shown next to your id   /quit");
            }
            app.note("PageUp/PageDown scroll, End follows the newest. Others' text is untrusted: never paste it into a command you don't understand.");
        }
        ("name", Some(n)) if !app.host_mode && !n.is_empty() => {
            app.name = Some(n.chars().take(32).collect());
            app.note(format!("you are now shown as {}", label(&app.me, app.name.as_deref())));
        }
        ("members", _) => {
            let lines: Vec<String> = app.members.iter().map(|m| format!("{} {}", if m.present { "●" } else { "○" }, m.id)).collect();
            for l in lines {
                app.note(l);
            }
        }
        ("link", _) if app.host_mode => return Action::Link,
        ("revoke", Some(p)) if app.host_mode && !p.is_empty() => return Action::Revoke(p.to_string()),
        (c, _) => app.note(format!("unknown command /{c}; try /help")),
    }
    Action::None
}

struct Terminal;

impl Terminal {
    fn enter() -> (Self, ratatui::DefaultTerminal) {
        let t = ratatui::init();
        (Terminal, t)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// Show an invite, keep it in `last-invite.txt` (a bordered pane is a poor place to copy a long link from)
/// and ask the terminal to put it on the clipboard (OSC 52; terminals that do not support it ignore it).
fn publish_invite(app: &mut App, link: &str) {
    use base64::Engine;
    use std::io::Write;
    let path = playbox_core::store::home().join("last-invite.txt");
    let saved = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new("."))).and_then(|_| std::fs::write(&path, format!("{link}\n")));
    #[cfg(unix)]
    if saved.is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let osc = format!("\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(link));
    let _ = std::io::stdout().write_all(osc.as_bytes()).and_then(|_| std::io::stdout().flush());
    app.note(format!("invite (1 hour, 1 use) copied if your terminal allows it, and saved to {}", path.display()));
    app.note(link.to_string());
}

pub async fn run(backend: Backend, title: String, name: Option<String>, first_invite: Option<String>) -> Result<()> {
    let host_mode = matches!(backend, Backend::Host(_));
    let me = backend.me();
    let mut app = App::new(title, me, name, host_mode);
    app.note("Welcome. /help lists commands.");
    if let Some(l) = first_invite {
        publish_invite(&mut app, &l);
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let poller = {
        let (b, tx) = (backend.clone(), tx.clone());
        tokio::spawn(async move {
            let mut since = 0u64;
            loop {
                let t0 = std::time::Instant::now();
                let wait = match tokio::time::timeout(POLL_TIMEOUT, b.feed(since)).await {
                    Ok(Ok(f)) => {
                        since = f.next;
                        let _ = tx.send(Event::Feed(f, t0.elapsed()));
                        POLL_EVERY
                    }
                    Ok(Err(e)) => {
                        let text = format!("{e:#}");
                        if text.contains("grant revoked") {
                            let _ = tx.send(Event::Down("removed from the room by the host".into()));
                            break;
                        }
                        let _ = tx.send(Event::Down(text));
                        Duration::from_secs(2)
                    }
                    Err(_) => {
                        let _ = tx.send(Event::Down("no answer from the host".into()));
                        Duration::from_secs(2)
                    }
                };
                tokio::time::sleep(wait).await;
            }
        })
    };
    let (say_tx, mut say_rx) = mpsc::unbounded_channel::<(String, Option<String>)>();
    let sender = {
        let (b, tx) = (backend.clone(), tx.clone());
        tokio::spawn(async move {
            while let Some((text, name)) = say_rx.recv().await {
                if let Err(e) = b.say(&text, name.as_deref()).await {
                    let _ = tx.send(Event::Note(format!("not sent: {e:#}")));
                }
            }
        })
    };

    let (_guard, mut terminal) = Terminal::enter();
    while !app.quit {
        terminal.draw(|f| draw(f, &app))?;
        while let Ok(ev) = rx.try_recv() {
            app.apply(ev);
        }
        // `block_in_place` keeps the wait for a key from stalling the other tasks on this thread.
        let key = tokio::task::block_in_place(|| -> Result<Option<TermEvent>> { Ok(if event::poll(Duration::from_millis(60))? { Some(event::read()?) } else { None }) })?;
        let Some(TermEvent::Key(k)) = key else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match (k.code, k.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) => app.quit = true,
            (KeyCode::Enter, _) => match submit(&mut app) {
                Action::None => {}
                Action::Say(t) => {
                    let _ = say_tx.send((t, app.name.clone()));
                }
                Action::Link => match &backend {
                    Backend::Host(h) => match h.create_link_with(Duration::from_secs(3600), 1, Default::default()) {
                        Ok(l) => publish_invite(&mut app, &l.encode()),
                        Err(e) => app.note(format!("could not make a link: {e:#}")),
                    },
                    Backend::Guest(_) => {}
                },
                Action::Revoke(prefix) => {
                    let found: Vec<_> = app.members.iter().filter(|m| m.id != "host" && m.id.starts_with(&prefix)).map(|m| m.id.clone()).collect();
                    match (found.as_slice(), &backend) {
                        ([id], Backend::Host(h)) => match id.parse::<playbox_net::EndpointId>() {
                            Ok(eid) => {
                                let done = h.revoke_member(&eid);
                                app.note(if done { format!("revoked {}", short(id)) } else { "not a current member".into() });
                            }
                            Err(e) => app.note(format!("bad id: {e}")),
                        },
                        ([], _) => app.note("no member starts with that"),
                        _ => app.note("that prefix matches several members; type more of it"),
                    }
                }
            },
            (KeyCode::Backspace, _) => {
                app.input.pop();
            }
            (KeyCode::PageUp, _) => app.scroll += 10,
            (KeyCode::PageDown, _) => app.scroll = app.scroll.saturating_sub(10),
            (KeyCode::End, _) => app.scroll = 0,
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) && app.input.chars().count() < 1000 => app.input.push(c),
            _ => {}
        }
    }
    poller.abort();
    sender.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal as T};

    fn say(seq: u64, from: &str, name: Option<&str>, text: &str) -> Item {
        Item { seq, time_ms: 1_700_000_000_000, kind: "say".into(), from: from.into(), name: name.map(Into::into), text: text.into() }
    }

    fn screen(app: &App, w: u16, h: u16) -> String {
        let mut t = T::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let b = t.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| b[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn shows_messages_events_members_and_status() {
        let mut app = App::new("room f0f2".into(), "aaaa1111".into(), Some("me".into()), false);
        app.apply(Event::Feed(
            Feed {
                items: vec![
                    say(1, "bbbb2222", Some("claude"), "hello from a closed model"),
                    Item { seq: 2, time_ms: 1_700_000_001_000, kind: "event".into(), from: "bbbb2222".into(), name: None, text: "task ok sh exit 0".into() },
                    say(3, "host", Some("host"), "welcome"),
                ],
                next: 3,
                now_ms: 0,
                members: vec![Presence { id: "aaaa1111".into(), name: Some("me".into()), present: true, last_seen_ms: 1 }, Presence { id: "bbbb2222".into(), name: Some("claude".into()), present: false, last_seen_ms: 0 }],
            },
            Duration::from_millis(42),
        ));
        let s = screen(&app, 100, 20);
        assert!(s.contains("hello from a closed model"), "{s}");
        assert!(s.contains("claude·bbbb"));
        assert!(s.contains("• task ok sh exit 0 (bbbb)"), "{s}");
        assert!(s.contains("welcome"));
        assert!(s.contains("live 42 ms"));
        assert!(s.contains("me·aaaa (you)"));
        assert!(s.contains("here 1/2"));
    }

    #[test]
    fn narrow_terminals_drop_the_sidebar_and_long_words_wrap() {
        let mut app = App::new("r".into(), "aaaa".into(), None, false);
        app.items.push(say(1, "bbbb", None, &"x".repeat(200)));
        let s = screen(&app, 40, 12);
        assert!(!s.contains("here "));
        assert!(s.lines().filter(|l| l.contains("xxxx")).count() >= 5);
    }

    #[test]
    fn a_down_host_is_shown_not_hidden() {
        let mut app = App::new("r".into(), "aaaa".into(), None, false);
        app.apply(Event::Down("connection lost".into()));
        assert!(screen(&app, 80, 10).contains("reconnecting: connection lost"));
    }

    #[test]
    fn typing_commands_and_plain_text() {
        let mut app = App::new("r".into(), "aaaa1111".into(), None, false);
        app.input = "hello team".into();
        assert!(matches!(submit(&mut app), Action::Say(t) if t == "hello team"));
        assert!(app.input.is_empty());
        app.input = "/name Ada".into();
        assert!(matches!(submit(&mut app), Action::None));
        assert_eq!(app.name.as_deref(), Some("Ada"));
        app.input = "/link".into();
        assert!(matches!(submit(&mut app), Action::None), "guests cannot mint links");
        app.input = "/nope".into();
        submit(&mut app);
        assert!(app.items.last().unwrap().text.contains("unknown command"));
        let mut host = App::new("r".into(), "host".into(), None, true);
        host.input = "/revoke bb".into();
        assert!(matches!(submit(&mut host), Action::Revoke(p) if p == "bb"));
        host.input = "/quit".into();
        submit(&mut host);
        assert!(host.quit);
    }

    #[test]
    fn wrapping_keeps_words_whole_and_splits_long_ones() {
        assert_eq!(wrap("aaa bbb ccc", 8), vec!["aaa bbb", "ccc"]);
        assert_eq!(wrap("abcdefghijkl", 8), vec!["abcdefgh", "ijkl"]);
        assert_eq!(wrap("one\ntwo", 20), vec!["one", "two"]);
    }
}
