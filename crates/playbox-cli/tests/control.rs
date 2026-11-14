//! A running `playbox up --state DIR` publishes itself for the hub: a record plus a private socket that mints a join link.
#![cfg(unix)]
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn ask(sock: &std::path::Path, line: &str) -> String {
    let mut s = UnixStream::connect(sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    writeln!(s, "{line}").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    out.trim().to_string()
}

#[test]
fn a_host_publishes_itself_and_mints_links_for_the_hub() {
    let d = std::env::temp_dir().join(format!("pbctl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let (state, hosting, home) = (d.join("state"), d.join("hosting"), d.join("home"));
    std::fs::create_dir_all(&state).unwrap();
    let mut host = Command::new(env!("CARGO_BIN_EXE_playbox"))
        .args(["up", "--local", "--console", "--state"])
        .arg(&state)
        .env("PLAYBOX_HOME", &home)
        .env("PLAYBOX_HOSTING_DIR", &hosting)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    let record = loop {
        let found = std::fs::read_dir(&hosting).ok().and_then(|r| r.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "json")));
        if let Some(p) = found {
            break p;
        }
        assert!(Instant::now() < end, "the host never published itself");
        std::thread::sleep(Duration::from_millis(200));
    };
    let rec: serde_json::Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    let sock = std::path::PathBuf::from(rec["sock"].as_str().unwrap());
    assert_eq!(rec["name"], "state");
    assert_eq!(rec["id"].as_str().unwrap().len(), 16);
    assert!(!std::fs::read_to_string(&record).unwrap().contains("playbox://"), "a record never holds a link");
    assert_eq!(std::fs::metadata(&hosting).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(ask(&sock, "ping"), "ok");
    let (a, b) = (ask(&sock, "link"), ask(&sock, "link"));
    assert!(a.starts_with("playbox://") && b.starts_with("playbox://") && a != b, "each ask mints a fresh link");
    assert!(ask(&sock, "link not-a-guest-id").starts_with("error:"), "a bad guest id is refused");
    assert!(ask(&sock, "rm -rf /").starts_with("error:"));
    writeln!(host.stdin.as_mut().unwrap(), "quit").unwrap();
    let _ = host.wait();
    assert!(!record.exists() && !sock.exists(), "a host that quits cleans up after itself");
    let _ = std::fs::remove_dir_all(&d);
}
