//! `playbox mcp` (what `setup` registers) honours PLAYBOX_LIVE=1: hello, nonce handshake, then other members' new messages.

use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    time::{Duration, Instant},
};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    rx: std::sync::mpsc::Receiver<Value>,
    next: u64,
    pushed: Vec<Value>,
}

impl Mcp {
    fn start(home: &Path, live: bool) -> Self {
        Self::start_with_link(home, live, None)
    }

    fn start_with_link(home: &Path, live: bool, link_file: Option<&Path>) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbox"));
        cmd.arg("mcp")
            .env("PLAYBOX_HOME", home)
            .env_remove("PLAYBOX_LOG")
            .env_remove("PLAYBOX_LIVE")
            .env_remove("PLAYBOX_JOIN_LINK_FILE");
        if live {
            cmd.env("PLAYBOX_LIVE", "1");
        }
        if let Some(path) = link_file {
            cmd.env("PLAYBOX_JOIN_LINK_FILE", path);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in out.lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        let mut m = Self {
            child,
            stdin,
            rx,
            next: 1,
            pushed: Vec::new(),
        };
        let init = m.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
        assert_eq!(
            init["result"]["capabilities"]["experimental"]["claude/channel"].is_object(),
            live,
            "{init}"
        );
        writeln!(
            m.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .unwrap();
        m
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let v = self
                .rx
                .recv_timeout(until.saturating_duration_since(Instant::now()))
                .expect("reply");
            if v["id"] == id {
                return v;
            }
            self.pushed.push(v);
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        (
            r["result"]["isError"].as_bool().unwrap_or(true),
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    /// Wait up to `secs` for a pushed notification matching `pred`.
    fn wait_push(&mut self, secs: u64, pred: impl Fn(&Value) -> bool) -> Option<Value> {
        let until = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(i) = self.pushed.iter().position(&pred) {
                return Some(self.pushed.remove(i));
            }
            match self
                .rx
                .recv_timeout(until.saturating_duration_since(Instant::now()))
            {
                Ok(v) => self.pushed.push(v),
                Err(_) => return None,
            }
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-mcp-live-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn playbox_mcp_with_live_on_says_hello_then_pushes_room_messages_after_the_handshake() {
    let w = scratch("a");
    let mut host = Mcp::start(&w.join("h"), false);
    let (_, created) = host.tool(
        "playbox_create",
        json!({"transport": "local", "name": "live", "max_members": 4}),
    );
    let link = created["link"].clone();
    let mut guest = Mcp::start(&w.join("g"), true);
    let (err, joined) = guest.tool(
        "playbox_join",
        json!({"link": link, "workspace": w.join("ws")}),
    );
    assert!(!err, "{joined}");

    let hello = guest
        .wait_push(10, |v| v["params"]["meta"]["event"] == "hello")
        .expect("a hello notification");
    assert_eq!(hello["method"], "notifications/claude/channel");
    let nonce = hello["params"]["meta"]["nonce"]
        .as_str()
        .unwrap()
        .to_string();

    // Nothing is delivered before the handshake.
    host.tool("playbox_say", json!({"text": "said before ready"}));
    assert!(guest
        .wait_push(5, |v| v["params"]["meta"]["event"] == "message")
        .is_none());

    let (err, ok) = guest.tool("playbox_live_ready", json!({"nonce": nonce}));
    assert!(!err, "{ok}");
    assert_eq!(ok["connected"], true);

    host.tool("playbox_say", json!({"text": "hello from the host"}));
    let m = guest
        .wait_push(15, |v| v["params"]["meta"]["event"] == "message")
        .expect("the new room message");
    assert_eq!(m["method"], "notifications/claude/channel");
    let content = m["params"]["content"].as_str().unwrap();
    assert!(content.contains("hello from the host"), "{content}");
    assert!(!content.contains("said before ready"), "{content}");
    assert_eq!(m["params"]["meta"]["untrusted"], true);
}

#[test]
fn playbox_mcp_without_live_pushes_nothing() {
    let w = scratch("b");
    let mut m = Mcp::start(&w.join("g"), false);
    assert!(m.wait_push(3, |_| true).is_none());
}

#[test]
fn playbox_mcp_joins_from_playbox_join_link_file_env() {
    let w = scratch("c");
    let mut host = Mcp::start(&w.join("h"), false);
    let (err, created) = host.tool(
        "playbox_create",
        json!({"transport": "local", "name": "autojoin", "max_members": 4}),
    );
    assert!(!err, "{created}");
    let link = created["link"].as_str().expect("invite link");
    let link_file = w.join("join-link");
    std::fs::write(&link_file, link).unwrap();

    let mut guest = Mcp::start_with_link(&w.join("g"), false, Some(&link_file));
    let (err, status) = guest.tool("playbox_status", json!({}));
    assert!(!err, "{status}");
    assert!(
        status["joined"]["member_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{status}"
    );
    assert_eq!(status["joined"]["primary_host"], created["host_id"]);
}
