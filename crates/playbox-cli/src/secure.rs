//! `playbox secure`: check that this machine, your agents and the hub are set up securely, and keep checking.
//!
//! Three kinds of check, all read-only unless `--fix` is given (and then only permission fixes):
//!   * the machine: key and state permissions, the binary, disk encryption and firewall, signed history logs;
//!   * the agents: each coding agent's Playbox registration points at a real, unwritable, current binary;
//!   * the hub: the service, what it exposes, a few unauthenticated probes of the live hub, and a fuzz run.
//!
//! The fuzzer never touches your live hub. It starts a throwaway copy of the same hub code on a free port with an
//! empty state directory, pairs it, and throws malformed and random requests at it, unauthenticated and signed in.
//! A request is a problem if the hub answers 5xx, hangs, drops the connection, lets an unauthenticated caller in
//! anywhere but the public pages, sends a CORS header, or prints a stack trace. The run is deterministic for a seed,
//! so a problem can be reproduced with `--seed N`.

use crate::cli::SecureArgs;
use crate::hub::{self, Reach};
use crate::{service, setup, wizard};
use anyhow::Result;
use playbox_core::{attest, platform, store};
use serde::Serialize;
use std::io::{BufRead, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Info,
    Warn,
    Bad,
}

#[derive(Clone, Debug)]
pub enum Fix {
    Chmod(PathBuf, u32),
}

#[derive(Serialize, Clone, Debug)]
pub struct Check {
    pub group: &'static str,
    pub id: String,
    pub label: String,
    pub level: Level,
    pub detail: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix: String,
    pub fixable: bool,
    #[serde(skip)]
    pub action: Option<Fix>,
}

impl Check {
    fn new(group: &'static str, id: &str, label: &str, level: Level, detail: impl Into<String>) -> Self {
        Self { group, id: id.into(), label: label.into(), level, detail: detail.into(), fix: String::new(), fixable: false, action: None }
    }
    fn hint(mut self, fix: impl Into<String>) -> Self {
        self.fix = fix.into();
        self
    }
    fn act(mut self, a: Fix) -> Self {
        self.fixable = true;
        self.action = Some(a);
        self
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Violation {
    pub kind: String,
    pub request: String,
    pub status: u16,
    pub index: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct Fuzz {
    pub seed: u64,
    pub requests: usize,
    pub seconds: f64,
    pub signed_in: bool,
    pub violations: Vec<Violation>,
    pub note: String,
}

#[derive(Serialize, Debug)]
pub struct Report {
    pub version: u32,
    pub at: u64,
    pub playbox: String,
    pub score: u32,
    pub checks: Vec<Check>,
    pub fuzz: Option<Fuzz>,
}

pub struct Opts {
    pub fix: bool,
    pub fuzz: bool,
    pub deep: bool,
    pub seed: Option<u64>,
    pub online: bool,
    pub port: Option<u16>,
    pub hub_dir: Option<String>,
}

impl From<&SecureArgs> for Opts {
    fn from(a: &SecureArgs) -> Self {
        Self { fix: a.fix, fuzz: !a.no_fuzz, deep: a.deep, seed: a.seed, online: a.online, port: a.port, hub_dir: a.hub_dir.clone() }
    }
}

// ---------- small helpers ----------

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

#[cfg(unix)]
fn mode_of(p: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o777)
}
#[cfg(not(unix))]
fn mode_of(_: &Path) -> Option<u32> {
    None
}

fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (p, mode);
        Ok(())
    }
}

fn out(prog: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(prog).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn hub_state_dir() -> PathBuf {
    std::env::var_os("PLAYBOX_HUB_HOME").map_or_else(|| home_dir().join(".playbox-hub"), PathBuf::from)
}

/// A private file or directory: nothing for group or others. `want` is the mode to fix it to.
fn private(group: &'static str, id: &str, label: &str, p: &Path, want: u32, out: &mut Vec<Check>) {
    let Some(m) = mode_of(p) else { return };
    let level = if p.is_dir() { Level::Warn } else { Level::Bad }; // a open folder leaks names; an open key file leaks the key
    if m & 0o077 == 0 {
        out.push(Check::new(group, id, label, Level::Ok, format!("{} is private ({m:o})", p.display())));
    } else {
        out.push(
            Check::new(group, id, label, level, format!("{} is {m:o}: other accounts on this machine can read it", p.display()))
                .hint(format!("chmod {want:o} {}", p.display()))
                .act(Fix::Chmod(p.to_path_buf(), want)),
        );
    }
}

// ---------- machine ----------

fn machine_checks(out: &mut Vec<Check>) {
    let home = store::home();
    private("machine", "home", "State directory", &home, 0o700, out);
    for k in ["guest.key", "session.key", "approver.key"] {
        let p = home.join(k);
        if p.exists() {
            private("machine", &format!("key-{k}"), &format!("Key {k}"), &p, 0o600, out);
        }
    }
    let hub = hub_state_dir();
    private("machine", "hub-home", "Hub state directory", &hub, 0o700, out);
    for f in ["hub.json", "composio.key"] {
        let p = hub.join(f);
        if p.exists() {
            private("machine", &format!("hub-{f}"), &format!("Hub {f}"), &p, 0o600, out);
        }
    }
    // The service log holds one-time pairing links; the older installs logged into ~/Library/Logs.
    for p in [home.join("hub.log"), home_dir().join("Library/Logs/playbox-hub.log")] {
        if p.exists() {
            let m = mode_of(&p).unwrap_or(0o600);
            if m & 0o077 == 0 {
                out.push(Check::new("machine", "hub-log", "Hub log", Level::Ok, format!("{} is private", p.display())));
            } else {
                out.push(
                    Check::new("machine", "hub-log", "Hub log", Level::Warn, format!("{} is {m:o} and holds one-time pairing links (valid 10 minutes)", p.display()))
                        .hint(format!("chmod 600 {}", p.display()))
                        .act(Fix::Chmod(p, 0o600)),
                );
            }
        }
    }
    match platform::detect(&home) {
        Ok(Some(p)) => out.push(Check::new("machine", "attestation", "Hardware attestation", Level::Ok, format!("this machine can prove its own state with {}", p.kind()))),
        Ok(None) => out.push(Check::new("machine", "attestation", "Hardware attestation", Level::Warn, "no hardware evidence available: a room can only see that your keys are self-signed").hint("see docs/attestation.md for the Secure Enclave and TPM setups")),
        Err(e) => out.push(Check::new("machine", "attestation", "Hardware attestation", Level::Warn, format!("couldn't read it: {e}"))),
    }
    if std::env::var("PLAYBOX_APPROVERS").is_ok_and(|v| !v.is_empty()) {
        out.push(Check::new("machine", "approvers", "Approval enforcement", Level::Ok, "risky actions need your approver key (PLAYBOX_APPROVERS is set)"));
    } else {
        out.push(Check::new("machine", "approvers", "Approval enforcement", Level::Info, "advisory only: set PLAYBOX_APPROVERS to your `playbox approver-key` to make approvals required"));
    }
    if cfg!(target_os = "macos") {
        match out_of("fdesetup", &["status"]) {
            Some(t) if t.contains("On") => out.push(Check::new("machine", "disk", "Disk encryption", Level::Ok, "FileVault is on: your keys are encrypted at rest")),
            Some(_) => out.push(Check::new("machine", "disk", "Disk encryption", Level::Warn, "FileVault is off; anyone with the disk can read your keys").hint("System Settings, Privacy & Security, FileVault")),
            None => out.push(Check::new("machine", "disk", "Disk encryption", Level::Info, "couldn't read FileVault status")),
        }
        match out_of("/usr/libexec/ApplicationFirewall/socketfilterfw", &["--getglobalstate"]) {
            Some(t) if t.contains("enabled") => out.push(Check::new("machine", "firewall", "Firewall", Level::Ok, "the application firewall is on")),
            Some(_) => out.push(Check::new("machine", "firewall", "Firewall", Level::Warn, "the application firewall is off").hint("System Settings, Network, Firewall")),
            None => {}
        }
        match out_of("csrutil", &["status"]) {
            Some(t) if t.contains("enabled") => out.push(Check::new("machine", "sip", "System integrity", Level::Ok, "System Integrity Protection is on")),
            Some(_) => out.push(Check::new("machine", "sip", "System integrity", Level::Warn, "System Integrity Protection is off").hint("reboot to Recovery and run: csrutil enable")),
            None => {}
        }
    } else if let Some(t) = out_of("lsblk", &["-no", "TYPE"]) {
        if t.lines().any(|l| l.trim() == "crypt") {
            out.push(Check::new("machine", "disk", "Disk encryption", Level::Ok, "an encrypted volume is in use"));
        } else {
            out.push(Check::new("machine", "disk", "Disk encryption", Level::Warn, "no encrypted volume found; your keys are readable from the raw disk").hint("use LUKS full-disk encryption"));
        }
    }
}

fn out_of(prog: &str, args: &[&str]) -> Option<String> {
    out(prog, args)
}

fn binary_checks(opts: &Opts, out: &mut Vec<Check>) {
    let Ok(exe) = std::env::current_exe().and_then(std::fs::canonicalize) else { return };
    let s = exe.to_string_lossy();
    if s.contains("/target/") {
        out.push(Check::new("binary", "build-tree", "Installed binary", Level::Warn, format!("{s} is a build-tree binary; it changes whenever you rebuild")).hint("cargo install --path crates/playbox-cli --locked --force"));
    } else {
        out.push(Check::new("binary", "build-tree", "Installed binary", Level::Ok, format!("{s}")));
    }
    let bad_perm = mode_of(&exe).is_some_and(|m| m & 0o022 != 0) || exe.parent().and_then(mode_of).is_some_and(|m| m & 0o022 != 0);
    if bad_perm {
        out.push(Check::new("binary", "writable", "Binary permissions", Level::Bad, "the binary or its folder can be changed by other accounts; they could swap it for something else").hint(format!("chmod go-w {s} and its folder")));
    } else {
        out.push(Check::new("binary", "writable", "Binary permissions", Level::Ok, "only you can change it"));
    }
    let dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    if let Some(other) = wizard::shadowed(&dirs, &exe) {
        out.push(Check::new("binary", "shadow", "Which playbox runs", Level::Warn, format!("your shell runs {} first, not this one", other.display())).hint("remove the old file, then copy this one over it"));
    } else {
        out.push(Check::new("binary", "shadow", "Which playbox runs", Level::Ok, "the one on your PATH is this one"));
    }
    if opts.online {
        match out_of(&s, &["update", "--check", "--json"]).and_then(|t| serde_json::from_str::<serde_json::Value>(t.trim()).ok()) {
            Some(v) if v["update_available"] == true => out.push(Check::new("binary", "update", "Updates", Level::Warn, format!("playbox {} is available (you have {})", v["latest"].as_str().unwrap_or("?"), env!("CARGO_PKG_VERSION"))).hint("playbox update")),
            Some(_) => out.push(Check::new("binary", "update", "Updates", Level::Ok, "you are on the latest release")),
            None => out.push(Check::new("binary", "update", "Updates", Level::Info, "couldn't reach the release server")),
        }
    }
}

fn history_checks(out: &mut Vec<Check>) {
    let dir = store::home().join("history");
    let mut logs: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if logs.is_empty() {
        out.push(Check::new("history", "chains", "Signed history", Level::Info, "no history logs yet"));
        return;
    }
    logs.sort_by(|a, b| b.0.cmp(&a.0));
    logs.truncate(40);
    let pins = match platform::PinStore::load(&platform::pins_path(&store::home())) {
        Ok(p) => p,
        Err(e) => return out.push(Check::new("history", "chains", "Signed history", Level::Warn, format!("couldn't load pinned devices: {e}"))),
    };
    let Ok(boot) = platform::boot_reference_from_env() else { return };
    let Ok(vs) = platform::verifiers(pins, boot) else { return };
    let refs: Vec<&dyn attest::PlatformVerifier> = vs.iter().map(std::convert::AsRef::as_ref).collect();
    let (mut ok, mut broken) = (0, Vec::new());
    for (_, p) in &logs {
        match store::verify_deep(p, &refs) {
            Ok(r) if r.chain_error.is_none() && r.problems.is_empty() => ok += 1,
            Ok(r) => broken.push(format!("{}: {}", p.file_name().unwrap_or_default().to_string_lossy(), r.chain_error.clone().unwrap_or_else(|| r.problems.join("; ")))),
            Err(e) => broken.push(format!("{}: {e}", p.display())),
        }
    }
    if broken.is_empty() {
        out.push(Check::new("history", "chains", "Signed history", Level::Ok, format!("{ok} log(s): every signature and link checks out")));
    } else {
        out.push(Check::new("history", "chains", "Signed history", Level::Bad, format!("{} of {} log(s) failed: {}", broken.len(), logs.len(), broken.join(" | "))).hint("do not trust a failed log until you know why; `playbox verify <log>` shows the detail"));
    }
}

// ---------- agents ----------

fn json_at(text: &str, path: &[&str]) -> Option<serde_json::Value> {
    let mut v: serde_json::Value = serde_json::from_str(text).ok()?;
    for k in path {
        v = v.get(*k)?.clone();
    }
    Some(v)
}

pub fn claude_cmd(text: &str) -> Option<String> {
    json_at(text, &["mcpServers", "playbox", "command"])?.as_str().map(str::to_string)
}
pub fn opencode_cmd(text: &str) -> Option<String> {
    json_at(text, &["mcp", "playbox", "command"])?.as_array()?.first()?.as_str().map(str::to_string)
}
fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '"' || c == '\'').to_string()
}
pub fn codex_cmd(text: &str) -> Option<String> {
    let mut inside = false;
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            inside = t == "[mcp_servers.playbox]";
        } else if inside {
            if let Some(v) = t.strip_prefix("command") {
                return Some(unquote(v.trim_start().strip_prefix('=')?));
            }
        }
    }
    None
}
pub fn hermes_cmd(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let i = lines.iter().position(|l| l.trim() == "playbox:")?;
    lines[i + 1..].iter().take(6).find_map(|l| l.trim().strip_prefix("command:").map(unquote))
}

fn agent_checks(out: &mut Vec<Check>) {
    let home = home_dir();
    let me = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    for agent in setup::installed_agents() {
        let (cfg, read): (PathBuf, fn(&str) -> Option<String>) = match agent {
            "claude" => (home.join(".claude.json"), claude_cmd),
            "codex" => (home.join(".codex/config.toml"), codex_cmd),
            "opencode" => (home.join(".config/opencode/opencode.json"), opencode_cmd),
            _ => (home.join(".hermes/config.yaml"), hermes_cmd),
        };
        let id = format!("agent-{agent}");
        let label = agent.to_string();
        let Some(cmd) = std::fs::read_to_string(&cfg).ok().as_deref().and_then(read) else {
            out.push(Check::new("agents", &id, &label, Level::Warn, "installed but not connected to Playbox").hint(format!("playbox setup --only {agent}")));
            continue;
        };
        let p = PathBuf::from(&cmd);
        if !p.is_file() {
            out.push(Check::new("agents", &id, &label, Level::Bad, format!("registered to run {cmd}, which doesn't exist: the agent can't start the Playbox tools")).hint("playbox setup"));
            continue;
        }
        let writable = mode_of(&p).is_some_and(|m| m & 0o022 != 0) || p.parent().and_then(mode_of).is_some_and(|m| m & 0o022 != 0);
        if writable {
            out.push(Check::new("agents", &id, &label, Level::Bad, format!("{cmd} (or its folder) can be changed by other accounts; the agent would run whatever replaces it")).hint("chmod go-w on the file and its folder"));
            continue;
        }
        let cfg_open = mode_of(&cfg).is_some_and(|m| m & 0o022 != 0);
        let theirs = std::fs::canonicalize(&p).ok();
        let ver = out_of(&cmd, &["--version"]).unwrap_or_default();
        let level_detail = if cmd.contains("/target/") {
            Some(format!("registered to a build-tree binary ({cmd}); it changes on every rebuild"))
        } else if cfg_open {
            Some(format!("{} can be edited by other accounts; someone could change which program runs", cfg.display()))
        } else if me.is_some() && theirs.is_some() && me != theirs && !ver.contains(env!("CARGO_PKG_VERSION")) {
            Some(format!("runs {} but this is {}; the agent is on a different build", ver.trim(), env!("CARGO_PKG_VERSION")))
        } else {
            None
        };
        match level_detail {
            Some(d) => out.push(Check::new("agents", &id, &label, Level::Warn, d).hint("playbox setup")),
            None => out.push(Check::new("agents", &id, &label, Level::Ok, format!("runs {cmd}, a private, current binary"))),
        }
    }
}

// ---------- the hub ----------

fn lan_ip() -> Option<std::net::IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?; // a UDP connect sends nothing; it only picks the outgoing interface
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

pub struct Resp {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

/// Send raw bytes and read the whole answer. Err is "timeout" (no complete answer in time) or "refused".
pub fn raw(addr: SocketAddr, bytes: &[u8]) -> std::result::Result<Resp, &'static str> {
    // A burst of short connections can use up the machine's ephemeral ports (they linger ~15 s): wait for one rather than call it a refusal.
    let mut tries = 0;
    let mut s = loop {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(s) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => return Err("refused"),
            Err(_) if tries < 60 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(_) => return Err("refused"),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    s.set_write_timeout(Some(Duration::from_secs(3))).ok();
    let _ = s.write_all(bytes); // the hub may answer and close before reading everything (an oversize body)
    if !framed_exactly(bytes) {
        let _ = s.shutdown(Shutdown::Write); // so a body shorter than its Content-Length ends instead of waiting
    } // (otherwise let the hub close first: the side that closes first keeps the socket's 15 s TIME_WAIT, and a long run would run out of ports)
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 512 * 1024 {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err("timeout"),
            Err(_) => break, // a reset after some bytes still counts as an answer
        }
    }
    Ok(parse_resp(&buf))
}

/// Whether the request's body is exactly as long as its Content-Length says (or it has none).
fn framed_exactly(req: &[u8]) -> bool {
    let Some(i) = req.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
    let head = String::from_utf8_lossy(&req[..i]).to_lowercase();
    let body = req.len() - i - 4;
    match head.lines().find_map(|l| l.strip_prefix("content-length:")) {
        Some(v) => v.trim().parse::<usize>().is_ok_and(|n| n == body),
        None => body == 0,
    }
}

pub fn parse_resp(buf: &[u8]) -> Resp {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n");
    let (head, body) = match split {
        Some(i) => (&buf[..i], buf[i + 4..].to_vec()),
        None => (buf, Vec::new()),
    };
    let head = String::from_utf8_lossy(head).to_lowercase();
    let status = head.strip_prefix("http/").and_then(|r| r.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).unwrap_or(0);
    Resp { status, head, body }
}

pub fn build(method: &str, path: &str, headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut v = format!("{method} {path} HTTP/1.1\r\n").into_bytes();
    for (k, val) in headers {
        v.extend_from_slice(format!("{k}: {val}\r\n").as_bytes());
    }
    v.extend_from_slice(b"Connection: close\r\n\r\n");
    v.extend_from_slice(body);
    v
}

fn h(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

fn get(addr: SocketAddr, path: &str, extra: &[(String, String)]) -> Option<Resp> {
    let mut hs = vec![h("Host", &format!("127.0.0.1:{}", addr.port()))];
    hs.extend_from_slice(extra);
    raw(addr, &build("GET", path, &hs, b"")).ok()
}

/// Read-only probes of the running hub: nothing here changes its state, and only one request counts toward its sign-in throttle.
fn live_checks(port: u16, reach: &Reach, out: &mut Vec<Check>) {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let (running, detail) = service::status();
    if service::installed() {
        let u = service::unit_path();
        let open = mode_of(&u).is_some_and(|m| m & 0o022 != 0);
        out.push(if open {
            Check::new("hub", "service-file", "Service definition", Level::Bad, format!("{} can be changed by other accounts; they could make the hub run anything", u.display())).hint(format!("chmod go-w {}", u.display()))
        } else {
            Check::new("hub", "service-file", "Service definition", Level::Ok, format!("{} is only writable by you", u.display()))
        });
        out.push(Check::new("hub", "service", "Background service", if running { Level::Ok } else { Level::Warn }, format!("{detail} (starts at login)")).hint(if running { "" } else { "playbox hub --service install" }));
    } else {
        out.push(Check::new("hub", "service", "Background service", Level::Info, "not installed: the hub only runs while you keep it open").hint("playbox init"));
    }
    if !service::wait_healthy(port, Duration::from_millis(800)) {
        out.push(Check::new("hub", "running", "Hub", Level::Info, format!("not running on port {port}; live probes skipped")).hint("playbox hub"));
        return;
    }
    out.push(Check::new("hub", "running", "Hub", Level::Ok, format!("answering on 127.0.0.1:{port}")));
    let Some(root) = get(addr, "/", &[]) else { return };
    let missing: Vec<&str> = ["content-security-policy", "x-frame-options: deny", "x-content-type-options: nosniff", "referrer-policy"].into_iter().filter(|k| !root.head.contains(k)).collect();
    out.push(if missing.is_empty() && !root.head.contains("access-control-allow-origin") {
        Check::new("hub", "headers", "Browser protections", Level::Ok, "CSP, frame, sniffing and referrer protections are sent; no CORS opening")
    } else {
        Check::new("hub", "headers", "Browser protections", Level::Bad, format!("missing or unsafe headers: {}", missing.join(", "))).hint("update the hub")
    });
    let mut leaks = Vec::new();
    for p in ["/api/me", "/api/rooms", "/api/system", "/api/integrations", "/api/invites", "/x/grants"] {
        match get(addr, p, &[]) {
            Some(r) if r.status == 401 => {}
            Some(r) => leaks.push(format!("{p} -> {}", r.status)),
            None => leaks.push(format!("{p} -> no answer")),
        }
    }
    out.push(if leaks.is_empty() {
        Check::new("hub", "unauth", "Closed to strangers", Level::Ok, "every private route answers 401 without a session")
    } else {
        Check::new("hub", "unauth", "Closed to strangers", Level::Bad, format!("unexpected answers without a session: {}", leaks.join(", "))).hint("do not expose this hub; update it")
    });
    let evil = raw(addr, &build("POST", "/api/rooms", &[h("Host", &format!("127.0.0.1:{port}")), h("Origin", "https://evil.example"), h("Content-Type", "application/json"), h("Content-Length", "2")], b"{}"));
    out.push(match evil {
        Ok(r) if r.status == 403 => Check::new("hub", "origin", "Cross-site requests", Level::Ok, "a request from another site is refused"),
        Ok(r) => Check::new("hub", "origin", "Cross-site requests", Level::Bad, format!("a cross-site POST was answered {}", r.status)).hint("update the hub"),
        Err(e) => Check::new("hub", "origin", "Cross-site requests", Level::Warn, format!("no answer ({e})")),
    });
    // What the network can see. A hub meant for this computer should not answer on the LAN address.
    if let Some(ip) = lan_ip() {
        let exposed = TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(800)).is_ok();
        out.push(match (exposed, reach) {
            (true, Reach::Local) => Check::new("hub", "lan", "Network exposure", Level::Warn, format!("the hub answers on {ip}:{port}, so anyone on this network can reach it")).hint("restart without --lan"),
            (true, _) => Check::new("hub", "lan", "Network exposure", Level::Info, format!("also answers on {ip}:{port} (--lan); pairing there is plain HTTP")),
            (false, _) => Check::new("hub", "lan", "Network exposure", Level::Ok, "only answers on this computer"),
        });
    }
    if let Reach::Named { hostname, .. } = reach {
        let code = out_of("curl", &["-sS", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "8", &format!("https://{hostname}/api/me")]);
        out.push(match code.as_deref().map(str::trim) {
            Some("401") => Check::new("hub", "public", "Public address", Level::Ok, format!("https://{hostname} reaches the hub and private routes still answer 401")),
            Some(c @ ("302" | "403")) => Check::new("hub", "public", "Public address", Level::Ok, format!("https://{hostname} is behind an access gate ({c})")),
            Some(c) => Check::new("hub", "public", "Public address", Level::Warn, format!("https://{hostname}/api/me answered {c}, not 401")),
            None => Check::new("hub", "public", "Public address", Level::Info, "couldn't test the public address (curl missing?)"),
        });
    }
}

// ---------- the fuzzer ----------

/// A fixed request, and what a correct hub's answer to it looks like.
type Case = (&'static str, Req, Box<dyn Fn(&Resp) -> bool>);

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15); // splitmix64: nearby seeds must not give nearby streams
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self((z ^ (z >> 31)) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn pick<T: Copy>(&mut self, s: &[T]) -> T {
        s[self.below(s.len())]
    }
    pub fn pct(&mut self, p: usize) -> bool {
        self.below(100) < p
    }
}

const ROUTES: &[&str] = &[
    "/", "/pair", "/join", "/i/{id}", "/icon.svg", "/manifest.webmanifest", "/sw.js", "/auth.js", "/api/me", "/api/rooms", "/api/rooms/{id}", "/api/rooms/{id}/invite", "/api/rooms/{id}/grants",
    "/api/rooms/{id}/agent-tokens", "/api/invites", "/api/invites/{id}", "/api/knocks", "/api/knocks/{id}/approve", "/api/knocks/{id}/deny", "/api/knock", "/api/knock/out", "/api/knock/out/{id}",
    "/api/agents", "/api/agents/{id}/connect", "/api/integrations", "/api/integrations/key", "/api/integrations/connect", "/api/integrations/toolkits", "/api/integrations/accounts/{id}",
    "/api/grants/{id}", "/api/agent-tokens/{id}", "/api/devices", "/api/devices/pair", "/api/devices/{id}", "/api/sessions/{id}", "/api/push/subscribe", "/api/push/unsubscribe", "/api/attention",
    "/api/system", "/api/system/verify", "/r/{id}/api/state", "/r/{id}/api/tool", "/r/{id}/", "/x/grants", "/x/tools", "/x/execute", "/api/auth/state", "/api/auth/weak", "/api/auth/register/begin",
    "/api/auth/register/finish", "/api/auth/login/begin", "/api/auth/login/finish", "/api/auth/stepup/begin", "/api/auth/stepup/finish", "/api/auth/logout", "/api/invite/{id}", "/api/invite/{id}/knock",
    "/api/invite/{id}/poll",
];
const IDS: &[&str] = &["a", "abc123abc123", "0", "zzzzzzzzzzzz", "..", "%2e%2e", "é", "default", "00000000000000000000000000000000", "x"];
const SUFFIX: &[&str] = &["", "", "", "/", "//", "/..", "/%00", "?a=b", "?q=%ff", "?since=-1", "?toolkit=../x", "/.", "/%2e%2e/%2e%2e", ";x", "?q=\u{1F600}"];
const KEYS: &[&str] = &["label", "machine", "kind", "repo", "key", "toolkit", "tool", "account", "level", "token", "code", "name", "text", "arguments", "ttl", "uses", "approve", "guest", "id", "link", "ticket", "response", "endpoint", "keys", "invite_ttl_secs", "local"];
const STRS: &[&str] = &["", "A", "../../../etc/passwd", "\\u0000", "'; DROP TABLE x;--", "<script>alert(1)</script>", "${jndi:ldap://x/a}", "%s%s%s%n", "\u{1F600}", "-1", "NaN", "null", "read", "write", "admin", "gmail", "GMAIL_SEND_EMAIL", "http://169.254.169.254/", "file:///etc/passwd"];
const CTYPES: &[&str] = &["application/json", "application/json", "application/json; charset=utf-8", "text/plain", "", "multipart/form-data; boundary=x", "application/x-www-form-urlencoded"];

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn gen_json(r: &mut Rng, depth: usize) -> String {
    match r.below(if depth > 2 { 7 } else { 10 }) {
        0 => "null".into(),
        1 => "true".into(),
        2 => "false".into(),
        3 => r.pick(&["0", "-1", "3.14", "1e999", "99999999999999999999999", "-0", "2147483648", "1e-400"]).to_string(),
        4 | 5 => {
            let v = if r.pct(10) { "x".repeat(1 + r.below(5000)) } else { (r.pick(STRS)).to_string() };
            json_str(&v)
        }
        6 => json_str(r.pick(KEYS)),
        7 => format!("[{}]", (0..r.below(4)).map(|_| gen_json(r, depth + 1)).collect::<Vec<_>>().join(",")),
        _ => format!("{{{}}}", (0..r.below(5)).map(|_| format!("{}:{}", json_str(r.pick(KEYS)), gen_json(r, depth + 1))).collect::<Vec<_>>().join(",")),
    }
}

fn gen_body(r: &mut Rng) -> Vec<u8> {
    match r.below(10) {
        0 | 1 => Vec::new(),
        2..=6 => format!("{{{}}}", (0..1 + r.below(5)).map(|_| format!("{}:{}", json_str(r.pick(KEYS)), gen_json(r, 0))).collect::<Vec<_>>().join(",")).into_bytes(),
        7 => {
            let n = r.pick(&[50usize, 500, 5000, 30000]);
            format!("{}{}", "[".repeat(n), "]".repeat(n)).into_bytes()
        }
        8 => (0..r.below(1500)).map(|_| r.below(256) as u8).collect(),
        _ => {
            let full = gen_json(r, 0);
            full.as_bytes()[..full.len() / 2].to_vec()
        }
    }
}

fn encode_path(p: &str) -> String {
    let mut o = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/%?=&:;,+@!$'()*".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

pub struct Req {
    pub method: &'static str,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub cookie_valid: bool,
}

pub fn gen_request(r: &mut Rng, port: u16, cookie: Option<&str>) -> Req {
    let method = r.pick(&["GET", "GET", "GET", "POST", "POST", "POST", "POST", "DELETE", "PATCH", "PUT", "HEAD", "OPTIONS"]);
    let mut path = r.pick(ROUTES).replace("{id}", r.pick(IDS));
    path.push_str(r.pick(SUFFIX));
    let path = encode_path(&path);
    let body = if matches!(method, "POST" | "PATCH" | "PUT" | "DELETE") { gen_body(r) } else { Vec::new() };
    let mut hs = vec![h("Host", &if r.pct(5) { "evil.example".to_string() } else { format!("127.0.0.1:{port}") })];
    match r.below(4) {
        0 | 1 => {}
        2 => hs.push(h("Origin", &format!("http://127.0.0.1:{port}"))),
        _ => hs.push(h("Origin", "https://evil.example")),
    }
    if r.pct(10) {
        hs.push(h("Sec-Fetch-Site", r.pick(&["cross-site", "same-site", "same-origin", "none"])));
    }
    let ct = r.pick(CTYPES);
    if !ct.is_empty() {
        hs.push(h("Content-Type", ct));
    }
    let mut valid = false;
    match (cookie, r.below(10)) {
        (Some(c), 0..=5) => {
            hs.push(h("Cookie", &format!("pbs={c}")));
            valid = true;
        }
        (_, 6) => hs.push(h("Cookie", &format!("pbs={:016x}", r.next()))),
        (_, 7) => hs.push(h("Cookie", "pbs=; ;;=")),
        _ => {}
    }
    if r.pct(10) {
        hs.push(h("Authorization", &format!("Bearer pbx_{:012x}.{:032x}", r.next() & 0xffff_ffff_ffff, r.next())));
    }
    if r.pct(8) {
        hs.push(h(r.pick(&["X-Forwarded-For", "Cf-Connecting-Ip", "X-Real-Ip"]), &format!("203.0.113.{}", r.below(255))));
    }
    let cl = match r.below(40) {
        0 => "abc".to_string(),
        1 => "-5".to_string(),
        2 => "99999999999".to_string(),
        3 => (body.len() + 50).to_string(),
        _ => body.len().to_string(),
    };
    if method != "GET" && method != "HEAD" || !body.is_empty() {
        hs.push(h("Content-Length", &cl));
    }
    Req { method, path, headers: hs, body, cookie_valid: valid }
}

/// The routes anyone may reach without signing in.
pub fn is_public(path: &str) -> bool {
    let p = path.split(['?', '#', ';']).next().unwrap_or(""); // the hub reads the path before any ;params
    let p = format!("/{}", p.trim_start_matches('/')); // and Python's server folds leading slashes into one
    matches!(p.as_str(), "/" | "/pair" | "/join" | "/icon.svg" | "/icon-192.png" | "/icon-512.png" | "/icon-maskable.png" | "/manifest.webmanifest" | "/auth.js" | "/space.js" | "/sw.js")
        || p.starts_with("/i/")
        || p.starts_with("/api/invite/")
        || p.starts_with("/api/auth/")
}

/// What is wrong with this answer, if anything.
pub fn judge(req: &Req, resp: &std::result::Result<Resp, &'static str>) -> Option<(String, u16)> {
    let r = match resp {
        Err("timeout") => return Some(("hung: no answer within 3 s".into(), 0)),
        Err(_) => return Some(("refused the connection".into(), 0)),
        Ok(r) => r,
    };
    if r.status == 0 {
        return Some(("dropped the connection without an answer".into(), 0));
    }
    let supported = matches!(req.method, "GET" | "POST" | "DELETE" | "PATCH" | "HEAD");
    if r.status >= 500 && (supported || r.status != 501) {
        return Some((format!("server error {}", r.status), r.status));
    }
    if r.head.contains("access-control-allow-origin") {
        return Some(("opens the API to other sites (CORS header)".into(), r.status));
    }
    if (200..300).contains(&r.status) && !req.cookie_valid && !is_public(&req.path) {
        return Some(("answered 2xx to a caller who isn't signed in".into(), r.status));
    }
    None
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_or(7799, |a| a.port())
}

fn cookie_from(head: &str) -> Option<String> {
    let i = head.find("set-cookie: pbs=")? + "set-cookie: pbs=".len();
    let v: String = head[i..].chars().take_while(|c| !matches!(c, ';' | '\r' | '\n')).collect();
    (!v.is_empty()).then_some(v)
}

/// The exception line of each Python traceback in `text`.
pub fn tracebacks(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("Traceback (most recent call last)") {
            let end = lines[i + 1..].iter().position(|x| !x.starts_with(' ') && !x.is_empty()).map_or(lines.len() - 1, |p| i + 1 + p);
            out.push(lines[end].trim().to_string());
        }
    }
    out
}

/// A short, secret-free description of a request for a report.
fn describe(r: &Req) -> String {
    let b: String = String::from_utf8_lossy(&r.body).chars().take(60).filter(|c| !c.is_control()).collect();
    format!("{} {}{}", r.method, r.path.chars().take(80).collect::<String>(), if b.is_empty() { String::new() } else { format!("  body: {b}") })
}

/// Kills the throwaway hub when dropped, so a panic or an early return can't leave it running.
struct KillOnDrop(std::process::Child);
impl std::ops::Deref for KillOnDrop {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn fuzz_hub(opts: &Opts) -> Result<Fuzz> {
    let t0 = Instant::now();
    let script = hub::find_hub(opts.hub_dir.as_deref())?;
    let python = hub::on_path("python3").ok_or_else(|| anyhow::anyhow!("python3 not found"))?;
    let seed = opts.seed.unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64 & 0xffff_ffff));
    let n = if opts.deep { 20_000 } else { 1_500 };
    let tmp = std::env::temp_dir().join(format!("pbsec-{}-{seed}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let port = free_port();
    let mut child = Command::new(python)
        .arg(&script)
        .args(["--port", &port.to_string(), "--home"])
        .arg(tmp.join("hub"))
        .env("PLAYBOX_HOME", tmp.join("pb"))
        .env("PATH", hub::hub_path())
        .env("PLAYBOX_HUB_NO_MACHINE", "1") // this copy must not schedule `playbox secure` itself
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(KillOnDrop)?;
    let (sout, serr) = (Arc::new(Mutex::new(String::new())), Arc::new(Mutex::new(String::new())));
    for (src, sink) in [(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), sout.clone()), (child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>), serr.clone())] {
        if let Some(s) = src {
            std::thread::spawn(move || {
                for l in std::io::BufReader::new(s).lines().map_while(std::result::Result::ok) {
                    if let Ok(mut g) = sink.lock() {
                        g.push_str(&l);
                        g.push('\n');
                    }
                }
            });
        }
    }
    let finish = |child: &mut KillOnDrop| {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&tmp);
    };
    if !service::wait_healthy(port, Duration::from_secs(20)) {
        finish(&mut child);
        anyhow::bail!("the throwaway hub didn't start: {}", serr.lock().map(|g| g.lines().last().unwrap_or("").to_string()).unwrap_or_default());
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    // Sign the fuzzer in the way a person would on this machine: the one-time pairing code the hub printed.
    let mut cookie = None;
    for _ in 0..60 {
        let code = sout.lock().ok().and_then(|g| g.find("/pair#").map(|i| g[i + 6..].chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect::<String>()));
        if let Some(code) = code.filter(|c| c.len() > 8) {
            let body = format!("{{\"code\":\"{code}\",\"name\":\"playbox secure\"}}");
            let hs = [h("Host", &format!("127.0.0.1:{port}")), h("Origin", &format!("http://127.0.0.1:{port}")), h("Content-Type", "application/json"), h("Content-Length", &body.len().to_string())];
            cookie = raw(addr, &build("POST", "/api/auth/weak", &hs, body.as_bytes())).ok().and_then(|r| cookie_from(&r.head));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let violations: Arc<Mutex<Vec<Violation>>> = Arc::default();
    let next = AtomicUsize::new(0);
    let note = |v: &Arc<Mutex<Vec<Violation>>>, kind: String, request: String, status: u16, index: usize| {
        if let Ok(mut g) = v.lock() {
            g.push(Violation { kind, request, status, index });
        }
    };
    // A fixed battery first: each of these has one right answer.
    let host = format!("127.0.0.1:{port}");
    let battery: Vec<Case> = vec![
        ("path traversal must not read files", Req { method: "GET", path: "/static/../../../../etc/passwd".into(), headers: vec![h("Host", &host)], body: vec![], cookie_valid: false }, Box::new(|r| !String::from_utf8_lossy(&r.body).contains("root:"))),
        ("encoded traversal must not read files", Req { method: "GET", path: "/%2e%2e/%2e%2e/etc/passwd".into(), headers: vec![h("Host", &host)], body: vec![], cookie_valid: false }, Box::new(|r| !String::from_utf8_lossy(&r.body).contains("root:"))),
        ("CRLF in a path must not inject a header", Req { method: "GET", path: "/%0d%0aSet-Cookie:%20pwned=1".into(), headers: vec![h("Host", &host)], body: vec![], cookie_valid: false }, Box::new(|r| !r.head.contains("pwned"))),
        ("a 70 KB URL is refused cleanly", Req { method: "GET", path: format!("/{}", "a".repeat(70_000)), headers: vec![h("Host", &host)], body: vec![], cookie_valid: false }, Box::new(|r| r.status > 0 && r.status < 500)),
        ("a wrong pairing code is refused", Req { method: "POST", path: "/api/auth/weak".into(), headers: vec![h("Host", &host), h("Origin", &format!("http://{host}")), h("Content-Type", "application/json"), h("Content-Length", "29")], body: br#"{"code":"nope","name":"x"}   "#.to_vec(), cookie_valid: false }, Box::new(|r| r.status == 403 || r.status == 429)),
        ("a forged session cookie gets nothing", Req { method: "GET", path: "/api/me".into(), headers: vec![h("Host", &host), h("Cookie", "pbs=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")], body: vec![], cookie_valid: false }, Box::new(|r| r.status == 401)),
    ];
    for (i, (what, req, ok)) in battery.iter().enumerate() {
        let resp = raw(addr, &build(req.method, &req.path, &req.headers, &req.body));
        match &resp {
            Ok(r) if ok(r) => {}
            Ok(r) => note(&violations, (*what).to_string(), describe(req), r.status, i),
            Err(e) => note(&violations, format!("{what} ({e})"), describe(req), 0, i),
        }
    }
    let workers = 8;
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                let mut rng = Rng::new(seed ^ (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                let req = gen_request(&mut rng, port, cookie.as_deref());
                let resp = raw(addr, &build(req.method, &req.path, &req.headers, &req.body));
                if let Some((kind, status)) = judge(&req, &resp) {
                    note(&violations, kind, describe(&req), status, i);
                }
            });
        }
    });
    if !service::wait_healthy(port, Duration::from_secs(5)) {
        std::thread::sleep(Duration::from_millis(300));
        let why = match child.try_wait() {
            Ok(Some(st)) => format!("exited with {st}"),
            _ => "still running but not answering".to_string(),
        };
        let tail = serr.lock().map(|g| g.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" / ")).unwrap_or_default();
        note(&violations, "the hub died or hung under load".into(), format!("{why}; {tail}"), 0, n);
    }
    std::thread::sleep(Duration::from_millis(300));
    let tb = serr.lock().map(|g| tracebacks(&g)).unwrap_or_default();
    for t in &tb {
        note(&violations, "unhandled exception in the hub".into(), t.clone(), 500, n + 1);
    }
    finish(&mut child);
    let mut all = violations.lock().map(|g| g.clone()).unwrap_or_default();
    let rank = |k: &str| if k.starts_with("the hub died") { 0 } else if k.starts_with("unhandled") { 1 } else if k.starts_with("server error") { 2 } else { 3 };
    all.sort_by_key(|v| (rank(&v.kind), v.index));
    let mut seen = std::collections::HashSet::new();
    // once the hub is gone every later request is refused: that is one problem, not thousands
    all.retain(|v| seen.insert(if v.kind.starts_with("refused") || v.kind.starts_with("hung") { v.kind.clone() } else { format!("{}|{}", v.kind, v.request.split(['?', ' ']).take(2).collect::<Vec<_>>().join(" ")) }));
    all.truncate(25);
    Ok(Fuzz { seed, requests: n + battery.len(), seconds: t0.elapsed().as_secs_f64(), signed_in: cookie.is_some(), violations: all, note: if cookie.is_some() { String::new() } else { "couldn't sign the fuzzer in; only the unauthenticated surface was tested".into() } })
}

// ---------- the report ----------

fn tally(checks: &[Check]) -> (usize, usize, usize) {
    let c = |l| checks.iter().filter(|x| x.level == l).count();
    (c(Level::Ok), c(Level::Warn), c(Level::Bad))
}

pub fn score(checks: &[Check], fuzz: &Option<Fuzz>) -> u32 {
    let (ok, warn, bad) = tally(checks);
    let (fok, fbad) = fuzz.as_ref().map_or((0, 0), |f| if f.violations.is_empty() { (1, 0) } else { (0, 1) });
    let total = ok + warn + bad + fok + fbad;
    if total == 0 { 0 } else { ((ok + fok) * 100 / total) as u32 }
}

pub fn collect(opts: &Opts) -> Report {
    let mut checks = Vec::new();
    machine_checks(&mut checks);
    binary_checks(opts, &mut checks);
    history_checks(&mut checks);
    agent_checks(&mut checks);
    let reach = hub::load().unwrap_or(Reach::Local);
    let port = opts.port.or_else(|| std::env::var("PLAYBOX_HUB_PORT").ok().and_then(|p| p.parse().ok())).unwrap_or(7717);
    live_checks(port, &reach, &mut checks);
    if opts.fix {
        for c in &mut checks {
            if let Some(Fix::Chmod(p, m)) = c.action.clone() {
                if set_mode(&p, m).is_ok() {
                    c.level = Level::Ok;
                    c.detail = format!("fixed: {} is now {m:o}", p.display());
                    c.fix.clear();
                    c.fixable = false;
                    c.action = None;
                }
            }
        }
    }
    let fuzz = if opts.fuzz {
        match fuzz_hub(opts) {
            Ok(f) => Some(f),
            Err(e) => {
                checks.push(Check::new("fuzz", "fuzz", "Fuzzing", Level::Info, format!("skipped: {e:#}")));
                None
            }
        }
    } else {
        None
    };
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    Report { version: 1, at, playbox: env!("CARGO_PKG_VERSION").to_string(), score: score(&checks, &fuzz), checks, fuzz }
}

pub fn save(r: &Report) {
    let p = store::home().join("security.json");
    if let Ok(t) = serde_json::to_string_pretty(r) {
        if std::fs::create_dir_all(store::home()).is_ok() && std::fs::write(&p, t).is_ok() {
            let _ = set_mode(&p, 0o600);
        }
    }
}

pub fn print(r: &Report) {
    let mut group = "";
    for c in &r.checks {
        if c.group != group {
            group = c.group;
            println!("\n{}", match group { "machine" => "This machine", "binary" => "Playbox itself", "history" => "History", "agents" => "Your agents", "hub" => "The hub", _ => "Fuzzing" });
        }
        let tag = match c.level { Level::Ok => " ok  ", Level::Info => "info ", Level::Warn => "WARN ", Level::Bad => "FAIL " };
        println!("  {tag} {:<22} {}", c.label, c.detail);
        if !c.fix.is_empty() {
            println!("        fix: {}{}", c.fix, if c.fixable { "   (or run with --fix)" } else { "" });
        }
    }
    if let Some(f) = &r.fuzz {
        println!("\nFuzzing a throwaway copy of the hub");
        println!("  {} {} requests in {:.1} s, seed {}, {}: {}", if f.violations.is_empty() { " ok  " } else { "FAIL " }, f.requests, f.seconds, f.seed,
            if f.signed_in { "signed in too" } else { "not signed in" }, if f.violations.is_empty() { "no problems".to_string() } else { format!("{} problem(s)", f.violations.len()) });
        for v in &f.violations {
            println!("        - {}: {} [{}] (repeat with --seed {}, request #{})", v.kind, v.request, v.status, f.seed, v.index);
        }
        if !f.note.is_empty() {
            println!("        note: {}", f.note);
        }
    }
    let (ok, warn, bad) = tally(&r.checks);
    let fz = r.fuzz.as_ref().map_or(0, |f| f.violations.len());
    println!("\n{ok} ok, {warn} to watch, {} failing. Score {}%.", bad + usize::from(fz > 0), r.score);
}

pub fn run(a: &SecureArgs) -> Result<u8> {
    let opts = Opts::from(a);
    if !a.json {
        println!("Playbox security check{}{}", if opts.fix { "  (fixing permissions)" } else { "" }, if opts.fuzz { "" } else { "  (fuzzing off)" });
    }
    let r = collect(&opts);
    save(&r);
    if a.json {
        println!("{}", serde_json::to_string(&r)?);
    } else {
        print(&r);
    }
    let (_, warn, bad) = tally(&r.checks);
    let failing = bad > 0 || r.fuzz.as_ref().is_some_and(|f| !f.violations.is_empty());
    Ok(if failing { 1 } else if a.strict && warn > 0 { 3 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_configs_are_read() {
        assert_eq!(claude_cmd(r#"{"mcpServers":{"playbox":{"command":"/a/playbox","args":["mcp"]}}}"#).as_deref(), Some("/a/playbox"));
        assert_eq!(claude_cmd(r#"{"mcpServers":{"other":{}}}"#), None);
        assert_eq!(opencode_cmd(r#"{"mcp":{"playbox":{"type":"local","command":["/b/playbox","mcp"]}}}"#).as_deref(), Some("/b/playbox"));
        assert_eq!(codex_cmd("[x]\ncommand = \"no\"\n[mcp_servers.playbox]\ncommand = \"/c/playbox\"\nargs = [\"mcp\"]\n").as_deref(), Some("/c/playbox"));
        assert_eq!(codex_cmd("[mcp_servers.other]\ncommand = \"/z\"\n"), None);
        assert_eq!(hermes_cmd("mcp_servers:\n  playbox:\n    command: \"/d/playbox\"\n    args: [\"mcp\"]\n").as_deref(), Some("/d/playbox"));
        assert_eq!(claude_cmd("not json"), None);
    }

    #[test]
    fn the_fuzzer_is_deterministic_and_varied() {
        let a: Vec<String> = (0..50).map(|i| describe(&gen_request(&mut Rng::new(7 ^ i), 1234, Some("c")))).collect();
        let b: Vec<String> = (0..50).map(|i| describe(&gen_request(&mut Rng::new(7 ^ i), 1234, Some("c")))).collect();
        assert_eq!(a, b, "same seed, same requests");
        assert!(a.iter().collect::<std::collections::HashSet<_>>().len() > 25, "and not all the same");
        for i in 0..300 {
            let r = gen_request(&mut Rng::new(i), 1, None);
            assert!(r.path.starts_with('/') && !r.path.contains(' ') && !r.path.contains('\n'), "{}", r.path);
        }
    }

    #[test]
    fn answers_are_judged() {
        let req = |m: &'static str, p: &str, valid| Req { method: m, path: p.into(), headers: vec![], body: vec![], cookie_valid: valid };
        let resp = |s: u16, head: &str| Ok(Resp { status: s, head: head.into(), body: vec![] });
        assert!(judge(&req("GET", "/api/me", false), &resp(401, "")).is_none());
        assert!(judge(&req("GET", "/api/me", false), &resp(200, "")).is_some(), "private route open to strangers");
        assert!(judge(&req("GET", "/api/me", true), &resp(200, "")).is_none(), "fine when signed in");
        assert!(judge(&req("GET", "/", false), &resp(200, "")).is_none(), "public page");
        assert!(judge(&req("GET", "/i/abc", false), &resp(200, "")).is_none());
        assert!(judge(&req("POST", "/api/rooms", true), &resp(500, "")).is_some());
        assert!(judge(&req("PUT", "/api/rooms", true), &resp(501, "")).is_none(), "unsupported method");
        assert!(judge(&req("GET", "/api/rooms", true), &resp(501, "")).is_some());
        assert!(judge(&req("GET", "/", false), &resp(200, "access-control-allow-origin: *")).is_some());
        assert!(judge(&req("GET", "/", false), &Err("timeout")).is_some());
        assert!(judge(&req("GET", "/", false), &resp(0, "")).is_some(), "dropped");
        assert!(is_public("/sw.js;x") && is_public("/api/invite/x?y=1") && is_public("/api/auth/weak") && !is_public("/api/invites") && !is_public("/x/execute"));
    }

    #[test]
    fn http_is_parsed() {
        assert!(framed_exactly(b"POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}"));
        assert!(!framed_exactly(b"POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\n{}"));
        assert!(!framed_exactly(b"POST / HTTP/1.1\r\nContent-Length: abc\r\n\r\n{}"));
        assert!(framed_exactly(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"));
        let r = parse_resp(b"HTTP/1.0 403 Forbidden\r\nSet-Cookie: pbs=abc123; HttpOnly\r\nContent-Length: 2\r\n\r\n{}");
        assert_eq!((r.status, r.body.as_slice()), (403, b"{}".as_slice()));
        assert_eq!(cookie_from(&r.head).as_deref(), Some("abc123"));
        assert_eq!(parse_resp(b"").status, 0);
        assert_eq!(parse_resp(b"garbage").status, 0);
    }

    #[test]
    fn tracebacks_are_summarised() {
        let t = "127.0.0.1 - GET /x\nTraceback (most recent call last):\n  File \"a.py\", line 1, in f\n    x\nRecursionError: maximum recursion depth exceeded\nlater line\n";
        assert_eq!(tracebacks(t), ["RecursionError: maximum recursion depth exceeded"]);
        assert!(tracebacks("nothing wrong").is_empty());
    }

    #[test]
    fn permissions_are_checked_and_fixed() {
        let d = std::env::temp_dir().join(format!("pbsec-t-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("k");
        std::fs::write(&f, "x").unwrap();
        set_mode(&f, 0o644).unwrap();
        let mut v = Vec::new();
        private("machine", "k", "Key", &f, 0o600, &mut v);
        assert_eq!((v[0].level, v[0].fixable), (Level::Bad, true));
        if let Some(Fix::Chmod(p, m)) = v[0].action.clone() {
            set_mode(&p, m).unwrap();
        }
        let mut v = Vec::new();
        private("machine", "k", "Key", &f, 0o600, &mut v);
        assert_eq!(v[0].level, Level::Ok);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_score_counts_fuzzing() {
        let ok = Check::new("g", "a", "A", Level::Ok, "");
        let bad = Check::new("g", "b", "B", Level::Bad, "");
        assert_eq!(score(&[ok.clone(), bad.clone()], &None), 50);
        let clean = Some(Fuzz { seed: 1, requests: 1, seconds: 0.0, signed_in: true, violations: vec![], note: String::new() });
        assert_eq!(score(std::slice::from_ref(&ok), &clean), 100);
        let dirty = Some(Fuzz { seed: 1, requests: 1, seconds: 0.0, signed_in: true, violations: vec![Violation { kind: "k".into(), request: "r".into(), status: 500, index: 0 }], note: String::new() });
        assert_eq!(score(&[ok], &dirty), 50);
    }
}
