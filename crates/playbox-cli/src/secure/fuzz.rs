//! Fuzzing a spawned hub with generated requests and collecting violations.

use super::gen::{gen_request, judge, Case, Req, Rng};
use super::http::{build, h, raw};
use super::{Fuzz, Opts, Violation};
use crate::hub::{self};
use crate::service;
use anyhow::Result;
use std::io::{BufRead, Read};
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_or(7799, |a| a.port())
}

pub(super) fn cookie_from(head: &str) -> Option<String> {
    let i = head.find("set-cookie: pbs=")? + "set-cookie: pbs=".len();
    let v: String = head[i..]
        .chars()
        .take_while(|c| !matches!(c, ';' | '\r' | '\n'))
        .collect();
    (!v.is_empty()).then_some(v)
}

/// The exception line of each Python traceback in `text`.
pub fn tracebacks(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("Traceback (most recent call last)") {
            let end = lines[i + 1..]
                .iter()
                .position(|x| !x.starts_with(' ') && !x.is_empty())
                .map_or(lines.len() - 1, |p| i + 1 + p);
            out.push(lines[end].trim().to_string());
        }
    }
    out
}

/// A short, secret-free description of a request for a report.
pub(super) fn describe(r: &Req) -> String {
    let b: String = String::from_utf8_lossy(&r.body)
        .chars()
        .take(60)
        .filter(|c| !c.is_control())
        .collect();
    format!(
        "{} {}{}",
        r.method,
        r.path.chars().take(80).collect::<String>(),
        if b.is_empty() {
            String::new()
        } else {
            format!("  body: {b}")
        }
    )
}

/// Kills the throwaway hub when dropped, so a panic or an early return can't leave it running.
pub(super) struct KillOnDrop(std::process::Child);
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
    let seed = opts.seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64 & 0xffff_ffff)
    });
    let n = if opts.deep { 20_000 } else { 1_500 };
    let tmp = std::env::temp_dir().join(format!("pbsec-{}-{seed}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let port = free_port();
    let mut child = Command::new(&python)
        .arg(&script)
        .args(["--port", &port.to_string(), "--home"])
        .arg(tmp.join("hub"))
        .env("PLAYBOX_HOME", tmp.join("pb"))
        .env("PATH", hub::hub_path())
        .env("PLAYBOX_HUB_NO_MACHINE", "1") // this copy must not schedule `playbox secure` itself
        .env("PYTHONFAULTHANDLER", "1") // so a hub that never answers can be asked where it is stuck (SIGABRT below)
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(KillOnDrop)?;
    let (sout, serr) = (
        Arc::new(Mutex::new(String::new())),
        Arc::new(Mutex::new(String::new())),
    );
    for (src, sink) in [
        (
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            sout.clone(),
        ),
        (
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            serr.clone(),
        ),
    ] {
        if let Some(s) = src {
            std::thread::spawn(move || {
                for l in std::io::BufReader::new(s)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
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
    let waited = Instant::now();
    if !service::wait_healthy(port, Duration::from_secs(20)) {
        let state = match child.try_wait() {
            Ok(Some(st)) => format!("exited with {st}"),
            Ok(None) => "still running but not answering".to_string(),
            Err(e) => format!("state unknown ({e})"),
        };
        // A hub that is up but silent is stuck somewhere. SIGABRT makes Python's fault handler print every thread's stack to stderr,
        // which is then part of the message, instead of a bare "not answering" that cannot be acted on.
        #[cfg(unix)]
        if state.starts_with("still running") {
            let _ = Command::new("kill")
                .args(["-ABRT", &child.id().to_string()])
                .status();
            std::thread::sleep(Duration::from_millis(1500));
        }
        let tail = |m: &Arc<Mutex<String>>| {
            m.lock()
                .map(|g| g.lines().rev().take(3).collect::<Vec<_>>().join(" | "))
                .unwrap_or_default()
        };
        let stack = |m: &Arc<Mutex<String>>| {
            m.lock()
                .map(|g| {
                    g.lines()
                        .filter(|l| {
                            l.contains("File \"")
                                || l.starts_with("Thread")
                                || l.starts_with("Current thread")
                        })
                        .take(24)
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default()
        };
        let (o, e) = (tail(&sout), tail(&serr));
        finish(&mut child);
        anyhow::bail!("the throwaway hub didn't start on port {port} after {:.0}s ({python:?}, hub {state}); stdout: {o}; stderr: {e}; where it was stuck: {}", waited.elapsed().as_secs_f32(), stack(&serr));
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    // Sign the fuzzer in the way a person would on this machine: the one-time pairing code the hub printed.
    let mut cookie = None;
    for _ in 0..60 {
        let code = sout.lock().ok().and_then(|g| {
            g.find("/pair#").map(|i| {
                g[i + 6..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                    .collect::<String>()
            })
        });
        if let Some(code) = code.filter(|c| c.len() > 8) {
            let body = format!("{{\"code\":\"{code}\",\"name\":\"playbox secure\"}}");
            let hs = [
                h("Host", &format!("127.0.0.1:{port}")),
                h("Origin", &format!("http://127.0.0.1:{port}")),
                h("Content-Type", "application/json"),
                h("Content-Length", &body.len().to_string()),
            ];
            cookie = raw(addr, &build("POST", "/api/auth/weak", &hs, body.as_bytes()))
                .ok()
                .and_then(|r| cookie_from(&r.head));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let violations: Arc<Mutex<Vec<Violation>>> = Arc::default();
    let next = AtomicUsize::new(0);
    let note = |v: &Arc<Mutex<Vec<Violation>>>,
                kind: String,
                request: String,
                status: u16,
                index: usize| {
        if let Ok(mut g) = v.lock() {
            g.push(Violation {
                kind,
                request,
                status,
                index,
            });
        }
    };
    // A fixed battery first: each of these has one right answer.
    let host = format!("127.0.0.1:{port}");
    // Malformed JSON, first of all: random fuzzing reaches the body parser through exactly one public route (and only while that route is
    // not yet rate-limited by the failures the other requests cause), so on a busy runner the hole could go unseen. These two always get there.
    let bad_json = |path: &str, signed: bool| {
        let mut hs = vec![
            h("Host", &host),
            h("Origin", &format!("http://{host}")),
            h("Content-Type", "application/json"),
            h("Content-Length", "9"),
        ];
        if let (true, Some(c)) = (signed, cookie.as_deref()) {
            hs.push(h("Cookie", &format!("pbs={c}")));
        }
        Req {
            method: "POST",
            path: path.into(),
            headers: hs,
            body: b"{not json".to_vec(),
            cookie_valid: signed && cookie.is_some(),
        }
    };
    let mut battery: Vec<Case> = vec![
        (
            "malformed JSON on a public route is a 400, not a crash",
            bad_json("/api/auth/login/begin", false),
            Box::new(|r| r.status > 0 && r.status < 500),
        ),
        (
            "malformed JSON on a signed-in route is a 400, not a crash",
            bad_json("/api/rooms", true),
            Box::new(|r| r.status > 0 && r.status < 500),
        ),
    ];
    battery.extend::<Vec<Case>>(vec![
        (
            "path traversal must not read files",
            Req {
                method: "GET",
                path: "/static/../../../../etc/passwd".into(),
                headers: vec![h("Host", &host)],
                body: vec![],
                cookie_valid: false,
            },
            Box::new(|r| !String::from_utf8_lossy(&r.body).contains("root:")),
        ),
        (
            "encoded traversal must not read files",
            Req {
                method: "GET",
                path: "/%2e%2e/%2e%2e/etc/passwd".into(),
                headers: vec![h("Host", &host)],
                body: vec![],
                cookie_valid: false,
            },
            Box::new(|r| !String::from_utf8_lossy(&r.body).contains("root:")),
        ),
        (
            "CRLF in a path must not inject a header",
            Req {
                method: "GET",
                path: "/%0d%0aSet-Cookie:%20pwned=1".into(),
                headers: vec![h("Host", &host)],
                body: vec![],
                cookie_valid: false,
            },
            Box::new(|r| !r.head.contains("pwned")),
        ),
        (
            "a 70 KB URL is refused cleanly",
            Req {
                method: "GET",
                path: format!("/{}", "a".repeat(70_000)),
                headers: vec![h("Host", &host)],
                body: vec![],
                cookie_valid: false,
            },
            Box::new(|r| r.status > 0 && r.status < 500),
        ),
        (
            "a wrong pairing code is refused",
            Req {
                method: "POST",
                path: "/api/auth/weak".into(),
                headers: vec![
                    h("Host", &host),
                    h("Origin", &format!("http://{host}")),
                    h("Content-Type", "application/json"),
                    h("Content-Length", "29"),
                ],
                body: br#"{"code":"nope","name":"x"}   "#.to_vec(),
                cookie_valid: false,
            },
            Box::new(|r| r.status == 403 || r.status == 429),
        ),
        (
            "a forged session cookie gets nothing",
            Req {
                method: "GET",
                path: "/api/me".into(),
                headers: vec![
                    h("Host", &host),
                    h("Cookie", "pbs=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
                ],
                body: vec![],
                cookie_valid: false,
            },
            Box::new(|r| r.status == 401),
        ),
    ]);
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
        let tail = serr
            .lock()
            .map(|g| {
                g.lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
        note(
            &violations,
            "the hub died or hung under load".into(),
            format!("{why}; {tail}"),
            0,
            n,
        );
    }
    std::thread::sleep(Duration::from_millis(300));
    let tb = serr.lock().map(|g| tracebacks(&g)).unwrap_or_default();
    for t in &tb {
        note(
            &violations,
            "unhandled exception in the hub".into(),
            t.clone(),
            500,
            n + 1,
        );
    }
    finish(&mut child);
    let mut all = violations.lock().map(|g| g.clone()).unwrap_or_default();
    let rank = |k: &str| {
        if k.starts_with("the hub died") {
            0
        } else if k.starts_with("unhandled") {
            1
        } else if k.starts_with("server error") {
            2
        } else {
            3
        }
    };
    all.sort_by_key(|v| (rank(&v.kind), v.index));
    let mut seen = std::collections::HashSet::new();
    // once the hub is gone every later request is refused: that is one problem, not thousands
    all.retain(|v| {
        seen.insert(
            if v.kind.starts_with("refused") || v.kind.starts_with("hung") {
                v.kind.clone()
            } else {
                format!(
                    "{}|{}",
                    v.kind,
                    v.request
                        .split(['?', ' '])
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            },
        )
    });
    all.truncate(25);
    Ok(Fuzz {
        seed,
        requests: n + battery.len(),
        seconds: t0.elapsed().as_secs_f64(),
        signed_in: cookie.is_some(),
        violations: all,
        note: if cookie.is_some() {
            String::new()
        } else {
            "couldn't sign the fuzzer in; only the unauthenticated surface was tested".into()
        },
    })
}

// ---------- the report ----------
