//! `playbox doctor`: what this machine can do today, and what is missing for the rest.
//!
//! The probes that start other programs (docker, the helper, openshell, uv) are slow when run one after another
//! (`docker info` alone can take a second), so they run together and the report is assembled in a fixed order.

use anyhow::Result;
use playbox_core::{platform, store};
use serde_json::json;
use std::process::{Command, ExitCode, Stdio};

struct Check {
    section: &'static str,
    label: String,
    ok: bool,
    detail: String,
}

fn runs(prog: &str, args: &[&str]) -> bool {
    Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Programs probed in parallel: (label, program, args).
const PROBES: &[(&str, &str, &[&str])] = &[
    ("docker", "docker", &["info"]),
    ("libkrun", "playbox-krun-helper", &["--help"]),
    ("openshell", "openshell", &["--help"]),
    ("steering image", "docker", &["image", "inspect", "playbox-steer:0.1"]),
    ("uv", "uv", &["--version"]),
];

fn probe_all() -> Vec<bool> {
    std::thread::scope(|s| {
        let handles: Vec<_> = PROBES
            .iter()
            .map(|(_, prog, args)| s.spawn(move || runs(prog, args)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or(false)).collect()
    })
}

fn collect() -> Vec<Check> {
    let mut out = Vec::new();
    let mut add = |section: &'static str, label: &str, ok: bool, detail: String| {
        out.push(Check { section, label: label.to_string(), ok, detail });
    };
    let probes = std::thread::spawn(probe_all);
    let home = store::home();

    for (label, file) in [("session key", "session.key"), ("approver key", "approver.key")] {
        match store::load_or_create_key(&home.join(file)) {
            Ok(k) if home.join(file).exists() => {
                add("identity", label, true, playbox_core::record::hex(&k.verifying_key().to_bytes()))
            }
            _ => add("identity", label, false, "not created yet".into()),
        }
    }
    match platform::PinStore::load(&platform::pins_path(&home)) {
        Ok(p) => add("identity", "pinned devices", true, p.list().len().to_string()),
        Err(e) => add("identity", "pinned devices", false, e.to_string()),
    }
    if std::env::var("PLAYBOX_APPROVERS").map(|v| !v.is_empty()).unwrap_or(false) {
        add("identity", "approval enforcement", true, "on (PLAYBOX_APPROVERS set)".into());
    } else {
        add(
            "identity",
            "approval enforcement",
            false,
            "advisory only; set PLAYBOX_APPROVERS to your `playbox approver-key`".into(),
        );
    }

    match platform::detect(&home) {
        Ok(Some(p)) => add("hardware evidence", "platform attestation", true, p.kind().to_string()),
        Ok(None) => add("hardware evidence", "platform attestation", false, "none; self-signed (T0) only".into()),
        Err(e) => add("hardware evidence", "platform attestation", false, e.to_string()),
    }

    let found = probes.join().unwrap_or_else(|_| vec![false; PROBES.len()]);
    let hint = |label: &str| match label {
        "docker" => "not running",
        "libkrun" => "playbox-krun-helper not found; see scripts/install-krun.sh",
        "steering image" => "build with: docker build -t playbox-steer:0.1 steer",
        _ => "not found",
    };
    for (i, (label, ..)) in PROBES.iter().enumerate().take(4) {
        let ok = found[i];
        let detail = match (ok, *label) {
            (true, "docker") => "running".to_string(),
            (true, "libkrun") => "helper found".to_string(),
            (true, "steering image") => "playbox-steer:0.1".to_string(),
            (true, _) => "found".to_string(),
            (false, l) => hint(l).to_string(),
        };
        add("sandboxes (for `up --runtime`)", label, ok, detail);
    }

    match crate::python::repo_root() {
        None => add("research tooling", "repository", false, "not found; run inside it or set PLAYBOX_ROOT".into()),
        Some(r) => {
            add("research tooling", "repository", true, r.display().to_string());
            for p in ["steer", "orchestrator", "world", "judge"] {
                if r.join(p).join(".venv").exists() {
                    add("research tooling", p, true, "venv ready".into());
                } else {
                    add("research tooling", p, false, format!("no venv; `cd {p} && uv sync`"));
                }
            }
        }
    }
    add("research tooling", "uv", found[4], if found[4] { "found" } else { "not found" }.into());
    out
}

pub fn run(as_json: bool) -> Result<ExitCode> {
    let checks = collect();
    let missing = checks.iter().filter(|c| !c.ok).count();
    if as_json {
        let items: Vec<_> = checks
            .iter()
            .map(|c| json!({"section": c.section, "check": c.label, "ok": c.ok, "detail": c.detail}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "version": env!("CARGO_PKG_VERSION"),
                "home": store::home().display().to_string(),
                "missing": missing,
                "checks": items,
            }))?
        );
    } else {
        println!("playbox {}  home {}", env!("CARGO_PKG_VERSION"), store::home().display());
        let mut section = "";
        for c in &checks {
            if c.section != section {
                section = c.section;
                println!("{section}");
            }
            println!("  {}    {:<22} {}", if c.ok { "ok" } else { "--" }, c.label, c.detail);
        }
    }
    // Missing optional pieces are normal (docker, libkrun, python tooling); only --json reports them via the status.
    Ok(if as_json && missing > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
}
