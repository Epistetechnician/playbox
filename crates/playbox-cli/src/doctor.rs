//! `playbox doctor`: what this machine can do today, and what is missing for the rest.

use anyhow::Result;
use playbox_core::{platform, store};
use std::process::{Command, Stdio};

fn ok(label: &str, detail: impl std::fmt::Display) {
    println!("  ok    {label:<22} {detail}");
}

fn miss(label: &str, detail: impl std::fmt::Display) {
    println!("  --    {label:<22} {detail}");
}

fn runs(prog: &str, args: &[&str]) -> bool {
    Command::new(prog)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn run() -> Result<()> {
    let home = store::home();
    println!(
        "playbox {}  home {}",
        env!("CARGO_PKG_VERSION"),
        home.display()
    );

    println!("identity");
    for (label, file) in [
        ("session key", "session.key"),
        ("approver key", "approver.key"),
    ] {
        match store::load_or_create_key(&home.join(file)) {
            Ok(k) if home.join(file).exists() => ok(
                label,
                playbox_core::record::hex(&k.verifying_key().to_bytes()),
            ),
            _ => miss(label, "not created yet"),
        }
    }
    match platform::PinStore::load(&platform::pins_path(&home)) {
        Ok(p) => ok("pinned devices", p.list().len()),
        Err(e) => miss("pinned devices", e),
    }
    if std::env::var("PLAYBOX_APPROVERS")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        ok("approval enforcement", "on (PLAYBOX_APPROVERS set)");
    } else {
        miss(
            "approval enforcement",
            "advisory only; set PLAYBOX_APPROVERS to your `playbox approver-key`",
        );
    }

    println!("hardware evidence");
    match platform::detect(&home) {
        Ok(Some(p)) => ok("platform attestation", p.kind()),
        Ok(None) => miss("platform attestation", "none; self-signed (T0) only"),
        Err(e) => miss("platform attestation", e),
    }

    println!("sandboxes (for `up --runtime`)");
    if runs("docker", &["info"]) {
        ok("docker", "running");
    } else {
        miss("docker", "not running");
    }
    if runs("playbox-krun-helper", &["--help"]) {
        ok("libkrun", "helper found");
    } else {
        miss(
            "libkrun",
            "playbox-krun-helper not found; see scripts/install-krun.sh",
        );
    }
    if runs("openshell", &["--help"]) {
        ok("openshell", "found");
    } else {
        miss("openshell", "not found");
    }
    if runs("docker", &["image", "inspect", "playbox-steer:0.1"]) {
        ok("steering image", "playbox-steer:0.1");
    } else {
        miss(
            "steering image",
            "build with: docker build -t playbox-steer:0.1 steer",
        );
    }

    println!("research tooling");
    match crate::python::repo_root() {
        None => miss("repository", "not found; run inside it or set PLAYBOX_ROOT"),
        Some(r) => {
            ok("repository", r.display());
            for p in ["steer", "orchestrator", "world", "judge"] {
                let venv = r.join(p).join(".venv").exists();
                if venv {
                    ok(p, "venv ready");
                } else {
                    miss(p, format!("no venv; `cd {p} && uv sync`"));
                }
            }
        }
    }
    if runs("uv", &["--version"]) {
        ok("uv", "found");
    } else {
        miss("uv", "not found");
    }
    Ok(())
}
