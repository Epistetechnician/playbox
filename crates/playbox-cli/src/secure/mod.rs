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
use anyhow::Result;
use playbox_core::store;
use serde::Serialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

mod agents;
mod fuzz;
mod gen;
mod http;
mod live;
mod machine;
pub(crate) use agents::hermes_cmd;
use agents::{agent_checks, binary_checks, history_checks};
pub use fuzz::fuzz_hub;
use live::live_checks;
use machine::{machine_checks, set_mode};

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
    fn new(
        group: &'static str,
        id: &str,
        label: &str,
        level: Level,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            group,
            id: id.into(),
            label: label.into(),
            level,
            detail: detail.into(),
            fix: String::new(),
            fixable: false,
            action: None,
        }
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
        Self {
            fix: a.fix,
            fuzz: !a.no_fuzz,
            deep: a.deep,
            seed: a.seed,
            online: a.online,
            port: a.port,
            hub_dir: a.hub_dir.clone(),
        }
    }
}

// ---------- small helpers ----------

fn tally(checks: &[Check]) -> (usize, usize, usize) {
    let c = |l| checks.iter().filter(|x| x.level == l).count();
    (c(Level::Ok), c(Level::Warn), c(Level::Bad))
}

pub fn score(checks: &[Check], fuzz: &Option<Fuzz>) -> u32 {
    let (ok, warn, bad) = tally(checks);
    let (fok, fbad) = fuzz.as_ref().map_or((0, 0), |f| {
        if f.violations.is_empty() {
            (1, 0)
        } else {
            (0, 1)
        }
    });
    let total = ok + warn + bad + fok + fbad;
    if total == 0 {
        0
    } else {
        ((ok + fok) * 100 / total) as u32
    }
}

pub fn collect(opts: &Opts) -> Report {
    let mut checks = Vec::new();
    machine_checks(&mut checks);
    binary_checks(opts, &mut checks);
    history_checks(&mut checks);
    agent_checks(&mut checks);
    let reach = hub::load().ok().flatten().unwrap_or(Reach::Local);
    let port = opts
        .port
        .or_else(|| {
            std::env::var("PLAYBOX_HUB_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
        })
        .unwrap_or(7717);
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
                checks.push(Check::new(
                    "fuzz",
                    "fuzz",
                    "Fuzzing",
                    Level::Info,
                    format!("skipped: {e:#}"),
                ));
                None
            }
        }
    } else {
        None
    };
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Report {
        version: 1,
        at,
        playbox: env!("CARGO_PKG_VERSION").to_string(),
        score: score(&checks, &fuzz),
        checks,
        fuzz,
    }
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
            println!(
                "\n{}",
                match group {
                    "machine" => "This machine",
                    "binary" => "Playbox itself",
                    "history" => "History",
                    "agents" => "Your agents",
                    "hub" => "The hub",
                    _ => "Fuzzing",
                }
            );
        }
        let tag = match c.level {
            Level::Ok => " ok  ",
            Level::Info => "info ",
            Level::Warn => "WARN ",
            Level::Bad => "FAIL ",
        };
        println!("  {tag} {:<22} {}", c.label, c.detail);
        if !c.fix.is_empty() {
            println!(
                "        fix: {}{}",
                c.fix,
                if c.fixable {
                    "   (or run with --fix)"
                } else {
                    ""
                }
            );
        }
    }
    if let Some(f) = &r.fuzz {
        println!("\nFuzzing a throwaway copy of the hub");
        println!(
            "  {} {} requests in {:.1} s, seed {}, {}: {}",
            if f.violations.is_empty() {
                " ok  "
            } else {
                "FAIL "
            },
            f.requests,
            f.seconds,
            f.seed,
            if f.signed_in {
                "signed in too"
            } else {
                "not signed in"
            },
            if f.violations.is_empty() {
                "no problems".to_string()
            } else {
                format!("{} problem(s)", f.violations.len())
            }
        );
        for v in &f.violations {
            println!(
                "        - {}: {} [{}] (repeat with --seed {}, request #{})",
                v.kind, v.request, v.status, f.seed, v.index
            );
        }
        if !f.note.is_empty() {
            println!("        note: {}", f.note);
        }
    }
    let (ok, warn, bad) = tally(&r.checks);
    let fz = r.fuzz.as_ref().map_or(0, |f| f.violations.len());
    println!(
        "\n{ok} ok, {warn} to watch, {} failing. Score {}%.",
        bad + usize::from(fz > 0),
        r.score
    );
}

pub fn run(a: &SecureArgs) -> Result<u8> {
    let opts = Opts::from(a);
    if !a.json {
        println!(
            "Playbox security check{}{}",
            if opts.fix {
                "  (fixing permissions)"
            } else {
                ""
            },
            if opts.fuzz { "" } else { "  (fuzzing off)" }
        );
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
    Ok(if failing {
        1
    } else if a.strict && warn > 0 {
        3
    } else {
        0
    })
}

#[cfg(test)]
mod tests {
    use super::agents::{claude_cmd, codex_cmd, hermes_cmd, opencode_cmd};
    use super::fuzz::{cookie_from, describe, tracebacks};
    use super::gen::{gen_request, is_public, judge, Req, Rng};
    use super::http::{framed_exactly, parse_resp, Resp};
    use super::machine::private;
    use super::*;

    #[test]
    fn agent_configs_are_read() {
        assert_eq!(
            claude_cmd(r#"{"mcpServers":{"playbox":{"command":"/a/playbox","args":["mcp"]}}}"#)
                .as_deref(),
            Some("/a/playbox")
        );
        assert_eq!(claude_cmd(r#"{"mcpServers":{"other":{}}}"#), None);
        assert_eq!(
            opencode_cmd(r#"{"mcp":{"playbox":{"type":"local","command":["/b/playbox","mcp"]}}}"#)
                .as_deref(),
            Some("/b/playbox")
        );
        assert_eq!(codex_cmd("[x]\ncommand = \"no\"\n[mcp_servers.playbox]\ncommand = \"/c/playbox\"\nargs = [\"mcp\"]\n").as_deref(), Some("/c/playbox"));
        assert_eq!(codex_cmd("[mcp_servers.other]\ncommand = \"/z\"\n"), None);
        assert_eq!(
            hermes_cmd(
                "mcp_servers:\n  playbox:\n    command: \"/d/playbox\"\n    args: [\"mcp\"]\n"
            )
            .as_deref(),
            Some("/d/playbox")
        );
        assert_eq!(claude_cmd("not json"), None);
    }

    #[test]
    fn the_fuzzer_is_deterministic_and_varied() {
        let a: Vec<String> = (0..50)
            .map(|i| describe(&gen_request(&mut Rng::new(7 ^ i), 1234, Some("c"))))
            .collect();
        let b: Vec<String> = (0..50)
            .map(|i| describe(&gen_request(&mut Rng::new(7 ^ i), 1234, Some("c"))))
            .collect();
        assert_eq!(a, b, "same seed, same requests");
        assert!(
            a.iter().collect::<std::collections::HashSet<_>>().len() > 25,
            "and not all the same"
        );
        for i in 0..300 {
            let r = gen_request(&mut Rng::new(i), 1, None);
            assert!(
                r.path.starts_with('/') && !r.path.contains(' ') && !r.path.contains('\n'),
                "{}",
                r.path
            );
        }
    }

    #[test]
    fn answers_are_judged() {
        let req = |m: &'static str, p: &str, valid| Req {
            method: m,
            path: p.into(),
            headers: vec![],
            body: vec![],
            cookie_valid: valid,
        };
        let resp = |s: u16, head: &str| {
            Ok(Resp {
                status: s,
                head: head.into(),
                body: vec![],
            })
        };
        assert!(judge(&req("GET", "/api/me", false), &resp(401, "")).is_none());
        assert!(
            judge(&req("GET", "/api/me", false), &resp(200, "")).is_some(),
            "private route open to strangers"
        );
        assert!(
            judge(&req("GET", "/api/me", true), &resp(200, "")).is_none(),
            "fine when signed in"
        );
        assert!(
            judge(&req("GET", "/", false), &resp(200, "")).is_none(),
            "public page"
        );
        assert!(judge(&req("GET", "/i/abc", false), &resp(200, "")).is_none());
        assert!(judge(&req("POST", "/api/rooms", true), &resp(500, "")).is_some());
        assert!(
            judge(&req("PUT", "/api/rooms", true), &resp(501, "")).is_none(),
            "unsupported method"
        );
        assert!(judge(&req("GET", "/api/rooms", true), &resp(501, "")).is_some());
        assert!(judge(
            &req("GET", "/", false),
            &resp(200, "access-control-allow-origin: *")
        )
        .is_some());
        assert!(judge(&req("GET", "/", false), &Err("timeout")).is_some());
        assert!(
            judge(&req("GET", "/", false), &resp(0, "")).is_some(),
            "dropped"
        );
        assert!(
            is_public("/sw.js;x")
                && is_public("/api/invite/x?y=1")
                && is_public("/api/auth/weak")
                && !is_public("/api/invites")
                && !is_public("/x/execute")
        );
    }

    #[test]
    fn http_is_parsed() {
        assert!(framed_exactly(
            b"POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}"
        ));
        assert!(!framed_exactly(
            b"POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\n{}"
        ));
        assert!(!framed_exactly(
            b"POST / HTTP/1.1\r\nContent-Length: abc\r\n\r\n{}"
        ));
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
        assert_eq!(
            tracebacks(t),
            ["RecursionError: maximum recursion depth exceeded"]
        );
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
        let clean = Some(Fuzz {
            seed: 1,
            requests: 1,
            seconds: 0.0,
            signed_in: true,
            violations: vec![],
            note: String::new(),
        });
        assert_eq!(score(std::slice::from_ref(&ok), &clean), 100);
        let dirty = Some(Fuzz {
            seed: 1,
            requests: 1,
            seconds: 0.0,
            signed_in: true,
            violations: vec![Violation {
                kind: "k".into(),
                request: "r".into(),
                status: 500,
                index: 0,
            }],
            note: String::new(),
        });
        assert_eq!(score(&[ok], &dirty), 50);
    }
}
