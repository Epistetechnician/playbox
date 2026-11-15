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

    let live = std::env::var("PLAYBOX_LIVE").is_ok_and(|v| matches!(v.as_str(), "1" | "on" | "true"));
    add(
        "live delivery",
        "channel push",
        live,
        if live {
            "on in this shell; the agent must answer the hello with playbox_live_ready (docs/live-delivery.md)".into()
        } else {
            "off; set PLAYBOX_LIVE=1 in the sidecar's environment to push room messages to a running agent".into()
        },
    );

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
                match venv_problem(&r.join(p).join(".venv")) {
                    None => add("research tooling", p, true, "venv ready".into()),
                    Some(why) => add("research tooling", p, false, format!("{why}; `cd {p} && uv sync`")),
                }
            }
        }
    }
    add("research tooling", "uv", found[4], if found[4] { "found" } else { "not found" }.into());
    out
}

/// Why a virtual environment cannot run Python, or `None` when it can. A `.venv` directory alone proves nothing: one copied
/// from another machine, or whose base interpreter was removed, has the folder and a dangling `python`.
fn venv_problem(venv: &std::path::Path) -> Option<String> {
    if !venv.is_dir() {
        return Some("no venv".into());
    }
    let python = ["bin/python", "Scripts/python.exe"].iter().map(|p| venv.join(p)).find(|p| p.exists());
    if python.is_none() {
        return Some("venv has no working python (a dangling link?)".into());
    }
    let cfg = std::fs::read_to_string(venv.join("pyvenv.cfg")).unwrap_or_default();
    let home = cfg.lines().find_map(|l| l.strip_prefix("home").and_then(|r| r.trim_start().strip_prefix('=')).map(|v| v.trim().to_string()));
    match home {
        Some(h) if !std::path::Path::new(&h).is_dir() => Some(format!("venv's base interpreter {h} is missing")),
        _ => None,
    }
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

#[cfg(test)]
mod venv_tests {
    use super::venv_problem;
    use std::fs;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("playbox-doctor-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("bin")).unwrap();
        d
    }

    #[test]
    fn a_venv_folder_without_a_working_python_is_not_ready() {
        let d = scratch("empty");
        assert!(venv_problem(&d).unwrap().contains("no working python"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_dangling_python_link_is_not_ready() {
        let d = scratch("dangling");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/nonexistent/python3", d.join("bin/python")).unwrap();
        #[cfg(unix)]
        assert!(venv_problem(&d).is_some());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_base_interpreter_is_not_ready_and_a_present_one_is() {
        let d = scratch("home");
        fs::write(d.join("bin/python"), "").unwrap();
        fs::write(d.join("pyvenv.cfg"), "home = /nonexistent/base/bin\n").unwrap();
        assert!(venv_problem(&d).unwrap().contains("missing"));
        fs::write(d.join("pyvenv.cfg"), format!("home = {}\n", d.join("bin").display())).unwrap();
        assert_eq!(venv_problem(&d), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn no_venv_at_all() {
        assert_eq!(venv_problem(std::path::Path::new("/nonexistent/.venv")), Some("no venv".into()));
    }
}
