//! Host and machine hardening checks: file modes, services and secrets on disk.

use super::{Check, Fix, Level};
use playbox_core::{platform, store};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(super) fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

#[cfg(unix)]
pub(super) fn mode_of(p: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}
#[cfg(not(unix))]
pub(super) fn mode_of(_: &Path) -> Option<u32> {
    None
}

pub(super) fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
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

pub(super) fn out(prog: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

pub(super) fn hub_state_dir() -> PathBuf {
    std::env::var_os("PLAYBOX_HUB_HOME")
        .map_or_else(|| home_dir().join(".playbox-hub"), PathBuf::from)
}

/// A private file or directory: nothing for group or others. `want` is the mode to fix it to.
pub(super) fn private(
    group: &'static str,
    id: &str,
    label: &str,
    p: &Path,
    want: u32,
    out: &mut Vec<Check>,
) {
    let Some(m) = mode_of(p) else { return };
    let level = if p.is_dir() { Level::Warn } else { Level::Bad }; // a open folder leaks names; an open key file leaks the key
    if m & 0o077 == 0 {
        out.push(Check::new(
            group,
            id,
            label,
            Level::Ok,
            format!("{} is private ({m:o})", p.display()),
        ));
    } else {
        out.push(
            Check::new(
                group,
                id,
                label,
                level,
                format!(
                    "{} is {m:o}: other accounts on this machine can read it",
                    p.display()
                ),
            )
            .hint(format!("chmod {want:o} {}", p.display()))
            .act(Fix::Chmod(p.to_path_buf(), want)),
        );
    }
}

// ---------- machine ----------

pub(super) fn machine_checks(out: &mut Vec<Check>) {
    let home = store::home();
    private("machine", "home", "State directory", &home, 0o700, out);
    for k in ["guest.key", "session.key", "approver.key"] {
        let p = home.join(k);
        if p.exists() {
            private(
                "machine",
                &format!("key-{k}"),
                &format!("Key {k}"),
                &p,
                0o600,
                out,
            );
        }
    }
    let hub = hub_state_dir();
    private(
        "machine",
        "hub-home",
        "Hub state directory",
        &hub,
        0o700,
        out,
    );
    for f in ["hub.json", "composio.key"] {
        let p = hub.join(f);
        if p.exists() {
            private(
                "machine",
                &format!("hub-{f}"),
                &format!("Hub {f}"),
                &p,
                0o600,
                out,
            );
        }
    }
    // The service log holds one-time pairing links; the older installs logged into ~/Library/Logs.
    for p in [
        home.join("hub.log"),
        home_dir().join("Library/Logs/playbox-hub.log"),
    ] {
        if p.exists() {
            let m = mode_of(&p).unwrap_or(0o600);
            if m & 0o077 == 0 {
                out.push(Check::new(
                    "machine",
                    "hub-log",
                    "Hub log",
                    Level::Ok,
                    format!("{} is private", p.display()),
                ));
            } else {
                out.push(
                    Check::new(
                        "machine",
                        "hub-log",
                        "Hub log",
                        Level::Warn,
                        format!(
                            "{} is {m:o} and holds one-time pairing links (valid 10 minutes)",
                            p.display()
                        ),
                    )
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
        out.push(Check::new(
            "machine",
            "approvers",
            "Approval enforcement",
            Level::Ok,
            "risky actions need your approver key (PLAYBOX_APPROVERS is set)",
        ));
    } else {
        out.push(Check::new("machine", "approvers", "Approval enforcement", Level::Info, "advisory only: set PLAYBOX_APPROVERS to your `playbox approver-key` to make approvals required"));
    }
    if cfg!(target_os = "macos") {
        match out_of("fdesetup", &["status"]) {
            Some(t) if t.contains("On") => out.push(Check::new(
                "machine",
                "disk",
                "Disk encryption",
                Level::Ok,
                "FileVault is on: your keys are encrypted at rest",
            )),
            Some(_) => out.push(
                Check::new(
                    "machine",
                    "disk",
                    "Disk encryption",
                    Level::Warn,
                    "FileVault is off; anyone with the disk can read your keys",
                )
                .hint("System Settings, Privacy & Security, FileVault"),
            ),
            None => out.push(Check::new(
                "machine",
                "disk",
                "Disk encryption",
                Level::Info,
                "couldn't read FileVault status",
            )),
        }
        match out_of(
            "/usr/libexec/ApplicationFirewall/socketfilterfw",
            &["--getglobalstate"],
        ) {
            Some(t) if t.contains("enabled") => out.push(Check::new(
                "machine",
                "firewall",
                "Firewall",
                Level::Ok,
                "the application firewall is on",
            )),
            Some(_) => out.push(
                Check::new(
                    "machine",
                    "firewall",
                    "Firewall",
                    Level::Warn,
                    "the application firewall is off",
                )
                .hint("System Settings, Network, Firewall"),
            ),
            None => {}
        }
        match out_of("csrutil", &["status"]) {
            Some(t) if t.contains("enabled") => out.push(Check::new(
                "machine",
                "sip",
                "System integrity",
                Level::Ok,
                "System Integrity Protection is on",
            )),
            Some(_) => out.push(
                Check::new(
                    "machine",
                    "sip",
                    "System integrity",
                    Level::Warn,
                    "System Integrity Protection is off",
                )
                .hint("reboot to Recovery and run: csrutil enable"),
            ),
            None => {}
        }
    } else if let Some(t) = out_of("lsblk", &["-no", "TYPE"]) {
        if t.lines().any(|l| l.trim() == "crypt") {
            out.push(Check::new(
                "machine",
                "disk",
                "Disk encryption",
                Level::Ok,
                "an encrypted volume is in use",
            ));
        } else {
            out.push(
                Check::new(
                    "machine",
                    "disk",
                    "Disk encryption",
                    Level::Warn,
                    "no encrypted volume found; your keys are readable from the raw disk",
                )
                .hint("use LUKS full-disk encryption"),
            );
        }
    }
}

pub(super) fn out_of(prog: &str, args: &[&str]) -> Option<String> {
    out(prog, args)
}
