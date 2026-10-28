//! Hardware attestation through the MCP server: a host with a real Secure Enclave key, a guest that
//! pins it (trust on first use) and then demands T1. macOS only; skipped without `swiftc`.
#![cfg(target_os = "macos")]

use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(home: &Path, env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbox-mcp"));
        cmd.env("PLAYBOX_HOME", home).env_remove("PLAYBOX_LOG").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn playbox-mcp");
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Self { child, stdin, out, next: 1 };
        m.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}));
        m
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn tool(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        (r["result"]["isError"].as_bool().unwrap_or(true), serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-mcp-plat-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn guest_pins_a_secure_enclave_host_then_requires_t1() {
    let work = scratch("se");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/playbox-se/playbox-se.swift");
    let helper = work.join("playbox-se");
    if !Command::new("swiftc").arg("-O").arg(&src).arg("-o").arg(&helper).status().map(|s| s.success()).unwrap_or(false) {
        eprintln!("skipping: swiftc not available");
        return;
    }
    let helper_s = helper.to_string_lossy().to_string();
    let host_env = [("PLAYBOX_ATTEST", "se"), ("PLAYBOX_SE_HELPER", helper_s.as_str())];

    let mut host = Mcp::start(&work.join("host"), &host_env);
    let (err, c) = host.tool("playbox_create", json!({"transport": "local", "name": "hw", "uses": 5}));
    assert!(!err, "{c}");
    let link = c["link"].clone();

    let guest_home = work.join("guest");
    let ws = |n: &str| work.join(n);
    let mut guest = Mcp::start(&guest_home, &[("PLAYBOX_ATTEST", "none")]);

    // Unpinned: the host's hardware evidence is valid but not trusted, so T1 is refused...
    let (err, body) = guest.tool("playbox_join", json!({"link": link, "min_host_tier": "t1", "workspace": ws("w1")}));
    assert!(err && body.as_str().unwrap().contains("achieves T0 but T1"), "{body}");
    assert!(body.as_str().unwrap().contains("not pinned"), "{body}");
    assert!(!guest_home.join("pins").exists(), "nothing is pinned without being asked");

    // ...trust on first use pins the device key and reaches T1 (the link was never spent).
    let (err, joined) = guest.tool("playbox_join", json!({"link": link, "min_host_tier": "t1", "trust_host_device": true, "workspace": ws("w2")}));
    assert!(!err, "{joined}");
    assert_eq!(joined["host_attestation"]["verified_tier"], "T1");
    assert_eq!(joined["host_attestation"]["device_newly_pinned"], true);
    let device = joined["host_attestation"]["device"].as_str().unwrap().to_string();
    assert!(device.starts_with("secure-enclave:"));
    let pins = std::fs::read_to_string(guest_home.join("pins")).unwrap();
    assert_eq!(pins.trim(), device.replacen(':', " ", 1));
    drop(guest);

    // The pin survives a restart: no flag needed, and the host is T1 again.
    let (_, c2) = host.tool("playbox_create", json!({"transport": "local", "name": "hw"}));
    let mut again = Mcp::start(&guest_home, &[("PLAYBOX_ATTEST", "none")]);
    let (err, j2) = again.tool("playbox_join", json!({"link": c2["link"], "min_host_tier": "t1", "workspace": ws("w3")}));
    assert!(!err, "{j2}");
    assert_eq!(j2["host_attestation"]["verified_tier"], "T1");
    assert_eq!(j2["host_attestation"]["device_newly_pinned"], false);

    // A host that has no hardware evidence cannot satisfy t1, even for a guest that trusts devices.
    let mut plain = Mcp::start(&work.join("plain"), &[("PLAYBOX_ATTEST", "none")]);
    let (_, pc) = plain.tool("playbox_create", json!({"transport": "local", "name": "plain"}));
    let mut g2 = Mcp::start(&work.join("g2"), &[("PLAYBOX_ATTEST", "none")]);
    let (err, body) = g2.tool("playbox_join", json!({"link": pc["link"], "min_host_tier": "t1", "trust_host_device": true, "workspace": ws("w4")}));
    assert!(err && body.as_str().unwrap().contains("achieves T0 but T1"), "{body}");
}
