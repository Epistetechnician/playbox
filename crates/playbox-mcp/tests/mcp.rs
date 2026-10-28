//! Drives two real `playbox-mcp` processes over stdio, the way an agent would.

use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

/// Two host/guest process pairs at once time out on connect on the dev Mac (cause not found),
/// so the tests that open rooms take turns.
static ROOMS: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(home: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_playbox-mcp"))
            .env("PLAYBOX_HOME", home)
            .env("PLAYBOX_ALLOW_UNSAFE_RUNNER", "1")
            .env_remove("PLAYBOX_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn playbox-mcp");
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Self { child, stdin, out, next: 1 };
        let init = m.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
        assert_eq!(init["result"]["serverInfo"]["name"], "playbox-mcp");
        m.notify("notifications/initialized");
        m
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(json!({"jsonrpc": "2.0", "method": method}));
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad reply {line:?}: {e}"));
        assert_eq!(v["id"], id, "reply id mismatch: {v}");
        v
    }

    /// Call a tool; returns (is_error, parsed JSON text or raw text).
    fn tool(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        let res = &r["result"];
        let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (res["isError"].as_bool().unwrap_or(true), body)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn protocol_basics() {
    let mut m = Mcp::start(&scratch("proto"));
    let list = m.rpc("tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in ["playbox_create", "playbox_join", "playbox_status", "playbox_ping", "playbox_pull", "playbox_delegate", "playbox_push", "playbox_verify", "playbox_revoke"] {
        assert!(names.contains(&want), "missing tool {want}");
    }
    for t in list["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object");
    }
    assert_eq!(m.rpc("ping", json!({}))["result"], json!({}));
    assert_eq!(m.rpc("no/such/method", json!({}))["error"]["code"], -32601);
    let (is_err, body) = m.tool("playbox_nope", json!({}));
    assert!(is_err && body.as_str().unwrap().contains("unknown tool"));
    // Using room tools before joining is a tool error, not a protocol error.
    let (is_err, body) = m.tool("playbox_ping", json!({"text": "x"}));
    assert!(is_err && body.as_str().unwrap().contains("playbox_join"));
    let (is_err, _) = m.tool("playbox_join", json!({}));
    assert!(is_err, "missing link must be refused");
}

#[test]
fn host_and_guest_collaborate_through_mcp() {
    let _turn = ROOMS.lock().unwrap_or_else(|e| e.into_inner());
    let work = scratch("collab");
    let repo = work.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "h@example.com"]);
    git(&repo, &["config", "user.name", "h"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let mut host = Mcp::start(&work.join("host-home"));
    let (err, created) = host.tool("playbox_create", json!({"repo": repo, "local": true, "unsafe_process_runner": true, "uses": 2}));
    assert!(!err, "{created}");
    let link = created["link"].as_str().unwrap().to_string();
    assert!(link.starts_with("playbox://"));

    let ws = work.join("guest-ws");
    let mut guest = Mcp::start(&work.join("guest-home"));
    let (err, joined) = guest.tool("playbox_join", json!({"link": link, "local": true, "workspace": ws}));
    assert!(!err, "{joined}");
    let member = joined["member_id"].as_str().unwrap().to_string();

    let (err, pong) = guest.tool("playbox_ping", json!({"text": "hi"}));
    assert!(!err && pong["echo"] == "hi");

    // Pull and check out.
    let (err, pulled) = guest.tool("playbox_pull", json!({"ref": "main", "checkout": true}));
    assert!(!err, "{pulled}");
    assert_eq!(std::fs::read_to_string(ws.join("README.md")).unwrap(), "hello\n");
    assert_eq!(pulled["head"], git(&repo, &["rev-parse", "main"]));

    // Delegate a command and apply the resulting patch.
    let (err, ran) = guest.tool("playbox_delegate", json!({"base": "main", "command": "echo built > out.txt && echo done", "apply": true, "timeout_secs": 20}));
    assert!(!err, "{ran}");
    assert_eq!(ran["exit_code"], 0);
    assert_eq!(ran["stdout"], "done\n");
    assert_eq!(ran["patch_applied"], true);
    assert!(ran["patch"].as_str().unwrap().contains("out.txt"));
    assert_eq!(std::fs::read_to_string(ws.join("out.txt")).unwrap(), "built\n");
    assert!(!repo.join("out.txt").exists(), "host checkout must stay untouched");

    // Commit locally and push to the member's own namespace.
    git(&ws, &["config", "user.email", "g@example.com"]);
    git(&ws, &["config", "user.name", "g"]);
    git(&ws, &["commit", "-q", "-m", "add out.txt"]);
    let (err, pushed) = guest.tool("playbox_push", json!({"branch": "main"}));
    assert!(!err, "{pushed}");
    let stored = pushed["stored_as"].as_str().unwrap();
    assert!(stored.starts_with("refs/playbox/members/"));
    assert_eq!(git(&repo, &["rev-parse", stored]), git(&ws, &["rev-parse", "main"]));
    assert_ne!(git(&repo, &["rev-parse", "main"]), git(&ws, &["rev-parse", "main"]), "host main must not move");

    // Status on both sides.
    let (_, hs) = host.tool("playbox_status", json!({}));
    assert_eq!(hs["hosting"]["members"], json!([member]));
    let (_, gs) = guest.tool("playbox_status", json!({}));
    assert_eq!(gs["joined"]["member_id"], json!(member));

    // The guest's own session history verifies, and covers every call above.
    let (err, v) = guest.tool("playbox_verify", json!({}));
    assert!(!err && v["valid"] == true, "{v}");
    // join, ping, pull, delegate, push, status: a call and a result each, plus this verify call.
    assert_eq!(v["entries"], 14, "{v}"); // +1: the host attestation recorded at join
    assert_eq!(v["attestations"][0]["about"], "host");
    assert_eq!(v["attestations"][0]["verified"], "T0");

    // Revocation reaches the guest on its next request.
    let (err, r) = host.tool("playbox_revoke", json!({"member": member}));
    assert!(!err && r["revoked"] == true);
    let (is_err, body) = guest.tool("playbox_ping", json!({"text": "again"}));
    assert!(is_err && body.as_str().unwrap().contains("revoked"), "{body}");
}

#[test]
fn guest_cannot_run_tasks_unless_host_enables_a_runner() {
    let _turn = ROOMS.lock().unwrap_or_else(|e| e.into_inner());
    let work = scratch("norunner");
    let repo = work.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "h@example.com"]);
    git(&repo, &["config", "user.name", "h"]);
    std::fs::write(repo.join("a"), "a").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "i"]);

    let mut host = Mcp::start(&work.join("h"));
    // Without the env opt-in the unsafe runner flag is ignored, and docker was not requested.
    let (_, created) = host.tool("playbox_create", json!({"repo": repo, "local": true}));
    let link = created["link"].as_str().unwrap().to_string();
    assert_eq!(created["members_may_run_tasks"], false);

    let mut guest = Mcp::start(&work.join("g"));
    let (err, _) = guest.tool("playbox_join", json!({"link": link, "local": true, "workspace": work.join("ws")}));
    assert!(!err);
    let (is_err, body) = guest.tool("playbox_delegate", json!({"base": "main", "command": "true"}));
    assert!(is_err && body.as_str().unwrap().contains("runs no tasks"), "{body}");
}

#[test]
fn a_room_comes_back_after_the_sidecar_restarts() {
    let _turn = ROOMS.lock().unwrap_or_else(|e| e.into_inner());
    let work = scratch("restart");
    let home = work.join("host-home");

    let mut host = Mcp::start(&home);
    let (err, first) = host.tool("playbox_create", json!({"transport": "local", "name": "team"}));
    assert!(!err, "{first}");
    assert_eq!(first["transport"], "local");
    let host_id = first["host_id"].as_str().unwrap().to_string();
    drop(host);

    let mut host = Mcp::start(&home);
    let (_, second) = host.tool("playbox_create", json!({"transport": "local", "name": "team"}));
    assert_eq!(second["host_id"], host_id.as_str(), "same room name must give the same identity");
    // One sidecar hosts one room; asking for another by name is an error, not a silent reuse.
    let (err, other) = host.tool("playbox_create", json!({"transport": "local", "name": "other"}));
    assert!(err && other.as_str().unwrap().contains("already hosts"), "{other}");

    // The guest follows the link's transport without being told.
    let mut guest = Mcp::start(&work.join("guest-home"));
    let (err, joined) = guest.tool("playbox_join", json!({"link": second["link"], "workspace": work.join("ws")}));
    assert!(!err, "{joined}");
    assert_eq!(guest.tool("playbox_ping", json!({"text": "x"})).1["echo"], "x");
}

#[test]
fn bad_transport_and_room_names_are_refused() {
    let mut m = Mcp::start(&scratch("badargs"));
    let (err, body) = m.tool("playbox_create", json!({"transport": "tor"}));
    assert!(err && body.as_str().unwrap().contains("transport must be"), "{body}");
    let (err, body) = m.tool("playbox_create", json!({"transport": "local", "name": "../escape"}));
    assert!(err && body.as_str().unwrap().contains("room name"), "{body}");
}

#[test]
fn joining_can_demand_a_tier_the_host_cannot_prove() {
    let _turn = ROOMS.lock().unwrap_or_else(|e| e.into_inner());
    let work = scratch("tier");
    let mut host = Mcp::start(&work.join("h"));
    let (_, created) = host.tool("playbox_create", json!({"transport": "local", "name": "t"}));
    let link = created["link"].clone();

    let mut guest = Mcp::start(&work.join("g"));
    let (err, body) = guest.tool("playbox_join", json!({"link": link, "min_host_tier": "t2", "workspace": work.join("ws")}));
    assert!(err && body.as_str().unwrap().contains("not joining") && body.as_str().unwrap().contains("T2"), "{body}");
    // The refused join never showed the invite to the host, so the same single-use link still works.
    let (err, ok) = guest.tool("playbox_join", json!({"link": link, "workspace": work.join("ws")}));
    assert!(!err, "{ok}");
    assert_eq!(ok["host_attestation"]["verified_tier"], "T0");
    assert_eq!(ok["host_attestation"]["claimed_tier"], "T0");
    assert_eq!(ok["host_attestation"]["binary_blake3"].as_str().unwrap().len(), 64);
}
