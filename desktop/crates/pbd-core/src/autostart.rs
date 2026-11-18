//! Launch at login, per operating system. macOS and Linux are plain files the user can read and delete; Windows uses the per-user Run key.
//! Nothing here needs administrator rights. All functions take the home directory so tests never touch the real one.

use crate::layout::Os;
use std::io;
use std::path::{Path, PathBuf};

pub const LABEL: &str = "com.playbox.desktop";

/// The file that enables autostart, or `None` where it is a registry value (Windows).
pub fn entry_path(os: Os, home: &Path) -> Option<PathBuf> {
    match os {
        Os::Mac => Some(home.join(format!("Library/LaunchAgents/{LABEL}.plist"))),
        Os::Linux => Some(home.join(".config/autostart/playbox.desktop")),
        Os::Windows => None,
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn render(os: Os, exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    match os {
        Os::Mac => format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n  <key>Label</key><string>{LABEL}</string>\n  <key>ProgramArguments</key><array><string>{}</string><string>--background</string></array>\n  <key>RunAtLoad</key><true/>\n  <key>ProcessType</key><string>Interactive</string>\n</dict></plist>\n",
            xml(&exe)
        ),
        _ => format!(
            "[Desktop Entry]\nType=Application\nName=Playbox\nExec=\"{}\" --background\nX-GNOME-Autostart-enabled=true\nTerminal=false\n",
            exe.replace('"', "\\\"")
        ),
    }
}

pub fn is_enabled(os: Os, home: &Path) -> bool {
    match entry_path(os, home) {
        Some(p) => p.is_file(),
        None => windows_run_key("query", None),
    }
}

pub fn enable(os: Os, home: &Path, exe: &Path) -> io::Result<()> {
    match entry_path(os, home) {
        Some(p) => {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(p, render(os, exe))
        }
        None => windows_run_key("add", Some(exe))
            .then_some(())
            .ok_or_else(|| io::Error::other("could not set the Windows Run key")),
    }
}

pub fn disable(os: Os, home: &Path) -> io::Result<()> {
    match entry_path(os, home) {
        Some(p) => match std::fs::remove_file(p) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        None => {
            windows_run_key("delete", None);
            Ok(())
        }
    }
}

/// Windows: `reg.exe` on HKCU\...\Run (per user, no elevation). A no-op returning false elsewhere.
fn windows_run_key(action: &str, exe: Option<&Path>) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let mut c = std::process::Command::new("reg");
    match action {
        "add" => {
            let v = format!(
                "\"{}\" --background",
                exe.map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default()
            );
            c.args(["add", key, "/v", "Playbox", "/t", "REG_SZ", "/d", &v, "/f"]);
        }
        "delete" => {
            c.args(["delete", key, "/v", "Playbox", "/f"]);
        }
        _ => {
            c.args(["query", key, "/v", "Playbox"]);
        }
    }
    c.output().map(|o| o.status.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pbd-auto-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn mac_writes_a_launch_agent_that_runs_at_load() {
        let h = tmp("mac");
        enable(
            Os::Mac,
            &h,
            Path::new("/Applications/Playbox.app/Contents/MacOS/playbox-desktop"),
        )
        .unwrap();
        let s = std::fs::read_to_string(entry_path(Os::Mac, &h).unwrap()).unwrap();
        assert!(s.contains("<key>RunAtLoad</key><true/>") && s.contains("--background"));
        assert!(is_enabled(Os::Mac, &h));
        disable(Os::Mac, &h).unwrap();
        assert!(!is_enabled(Os::Mac, &h));
        disable(Os::Mac, &h).unwrap(); // idempotent
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn linux_writes_an_xdg_autostart_entry() {
        let h = tmp("linux");
        enable(Os::Linux, &h, Path::new("/opt/playbox/playbox-desktop")).unwrap();
        let s = std::fs::read_to_string(entry_path(Os::Linux, &h).unwrap()).unwrap();
        assert!(
            s.starts_with("[Desktop Entry]")
                && s.contains("Exec=\"/opt/playbox/playbox-desktop\" --background")
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_path_with_markup_characters_cannot_break_the_plist() {
        let s = render(Os::Mac, Path::new("/Apps/A&B <x>/playbox"));
        assert!(s.contains("A&amp;B &lt;x&gt;") && !s.contains("<x>"));
    }

    #[test]
    fn windows_has_no_file_entry() {
        assert_eq!(entry_path(Os::Windows, Path::new("C:/u")), None);
    }
}
