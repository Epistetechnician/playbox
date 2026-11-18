//! OS deep links (`playbox://` and `web+playbox:`).
//!
//! 1. Validates an untrusted URL from the operating system and converts it into a safe hub path (`/join#<fragment>`) matching
//!    `home.html`'s invite/knock loader.
//! 2. Queues deep links in `<data_dir>/deeplinks/` (`0700`/`0600`) when a second `playbox-desktop` process is invoked on Linux or
//!    Windows while another instance already holds the single-instance claim.
//! 3. Registers per-user URL scheme handlers on Linux (`.desktop` + `xdg-mime`) and Windows (`HKCU\Software\Classes`) without
//!    needing administrator rights; on macOS, `Info.plist` (`CFBundleURLTypes`) handles LaunchServices registration.

use crate::layout::Os;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const QUEUE_DIR: &str = "deeplinks";
static SEQ: AtomicU64 = AtomicU64::new(0);

fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for c in bytes.chunks(3) {
        let n = c.iter().fold(0u32, |a, b| a << 8 | u32::from(*b)) << (8 * (3 - c.len()));
        for i in 0..=c.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Convert a `playbox://...` or `web+playbox:...` URL into a safe `/join#<fragment>` path for the hub.
pub fn parse_to_hub_path(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.len() > 4096 || s.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return None;
    }
    if let Some(rest) = strip_ascii_prefix_ignore_case(s, "playbox://") {
        if rest.is_empty()
            || !rest.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'_' | b'-' | b'+' | b'/' | b'=' | b'.' | b'#' | b':' | b'@'
                    )
            })
        {
            return None;
        }
        let canonical = format!("playbox://{rest}");
        return Some(format!("/join#{}", b64url(canonical.as_bytes())));
    }
    if let Some(mut rest) = strip_ascii_prefix_ignore_case(s, "web+playbox:") {
        if let Some(stripped) = rest.strip_prefix("//") {
            rest = stripped;
        }
        if rest.is_empty() {
            return None;
        }
        // If someone wrapped a raw playbox:// URL inside web+playbox:, handle that too.
        if strip_ascii_prefix_ignore_case(rest, "playbox://").is_some() {
            return parse_to_hub_path(rest);
        }
        if !rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'~' | b'.'))
        {
            return None;
        }
        return Some(format!("/join#{rest}"));
    }
    None
}

fn strip_ascii_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() < prefix.len() {
        return None;
    }
    let (head, tail) = s.split_at(prefix.len());
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

/// Extract valid hub `/join#...` paths from command-line arguments.
pub fn from_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .filter_map(|a| parse_to_hub_path(a.as_ref()))
        .collect()
}

#[cfg(unix)]
fn ensure_private_queue(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::other("deeplinks path is not a directory"));
    }
    if meta.uid() != rustix::process::getuid().as_raw() {
        return Err(io::Error::other(
            "deeplinks directory owned by another user",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_queue(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Enqueue a validated `/join#...` hub path for an already-running Playbox Desktop instance.
pub fn enqueue_hub_path(data_dir: &Path, hub_path: &str) -> io::Result<()> {
    if !hub_path.starts_with("/join#") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a /join# path",
        ));
    }
    let dir = data_dir.join(QUEUE_DIR);
    ensure_private_queue(&dir)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = format!("{now:020}-{}-{seq}.link", std::process::id());
    let tmp = dir.join(format!(".{name}.tmp"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(hub_path.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(hub_path.as_bytes())?;
    }
    std::fs::rename(&tmp, dir.join(name))
}

/// Drain any queued `/join#...` hub paths in arrival order.
pub fn drain_hub_paths(data_dir: &Path) -> Vec<String> {
    let dir = data_dir.join(QUEUE_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.starts_with('.') && n.ends_with(".link"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    let mut out = Vec::new();
    for p in files {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let ok = std::fs::symlink_metadata(&p)
                .map(|m| {
                    m.file_type().is_file()
                        && m.uid() == rustix::process::getuid().as_raw()
                        && (m.permissions().mode() & 0o077) == 0
                })
                .unwrap_or(false);
            if !ok {
                let _ = std::fs::remove_file(&p);
                continue;
            }
        }
        if let Ok(s) = std::fs::read_to_string(&p) {
            let trimmed = s.trim();
            if trimmed.starts_with("/join#")
                && trimmed.len() <= 6000
                && trimmed[6..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'~' | b'.'))
            {
                out.push(trimmed.to_string());
            }
        }
        let _ = std::fs::remove_file(&p);
    }
    out
}

pub fn linux_desktop_entry_path(home: &Path) -> PathBuf {
    home.join(".local/share/applications/playbox-desktop.desktop")
}

pub fn render_linux_desktop_entry(exe: &Path) -> String {
    let mut escaped = String::new();
    for ch in exe.to_string_lossy().chars() {
        if ch.is_control() {
            continue;
        }
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '$' => escaped.push_str("\\$"),
            '`' => escaped.push_str("\\`"),
            '%' => escaped.push_str("%%"),
            other => escaped.push(other),
        }
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=Playbox\nComment=Rooms where people and their agents work together\nExec=\"{escaped}\" %u\nTerminal=false\nCategories=Development;Utility;\nMimeType=x-scheme-handler/playbox;x-scheme-handler/web+playbox;\n"
    )
}

/// Register `playbox://` and `web+playbox:` for the current user (best-effort, no root/admin).
pub fn register(os: Os, home: &Path, exe: &Path) -> io::Result<()> {
    match os {
        Os::Mac => Ok(()), // Info.plist CFBundleURLTypes registers with LaunchServices automatically.
        Os::Linux => {
            let path = linux_desktop_entry_path(home);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, render_linux_desktop_entry(exe))?;
            if cfg!(target_os = "linux") {
                for mime in ["x-scheme-handler/playbox", "x-scheme-handler/web+playbox"] {
                    let _ = std::process::Command::new("xdg-mime")
                        .args(["default", "playbox-desktop.desktop", mime])
                        .output();
                }
            }
            Ok(())
        }
        Os::Windows => {
            if cfg!(windows) {
                let cmd = format!("\"{}\" \"%1\"", exe.to_string_lossy());
                for scheme in ["playbox", "web+playbox"] {
                    let root = format!(r"HKCU\Software\Classes\{scheme}");
                    let open = format!(r"{root}\shell\open\command");
                    let _ = std::process::Command::new("reg")
                        .args(["add", &root, "/ve", "/d", "URL:Playbox Invite", "/f"])
                        .output();
                    let _ = std::process::Command::new("reg")
                        .args(["add", &root, "/v", "URL Protocol", "/d", "", "/f"])
                        .output();
                    let _ = std::process::Command::new("reg")
                        .args(["add", &open, "/ve", "/d", &cmd, "/f"])
                        .output();
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playbox_and_web_playbox_links_and_rejects_hostile_inputs() {
        let p = parse_to_hub_path("playbox://join_token_123.abc#frag").unwrap();
        assert_eq!(
            p,
            format!("/join#{}", b64url(b"playbox://join_token_123.abc#frag"))
        );

        assert_eq!(
            parse_to_hub_path("web+playbox:a.eyJiIjoiaHR0cHMifQ").as_deref(),
            Some("/join#a.eyJiIjoiaHR0cHMifQ")
        );
        assert_eq!(
            parse_to_hub_path("web+playbox:cGxheWJveDovL3g~TGFi").as_deref(),
            Some("/join#cGxheWJveDovL3g~TGFi")
        );

        assert_eq!(parse_to_hub_path("javascript:alert(1)"), None);
        assert_eq!(parse_to_hub_path("https://example.com"), None);
        assert_eq!(parse_to_hub_path("playbox://bad\"quote"), None);
        assert_eq!(parse_to_hub_path("web+playbox:<script>"), None);
    }

    #[test]
    fn queue_enqueues_and_drains_in_order() {
        let dir = std::env::temp_dir().join(format!("pbd-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        enqueue_hub_path(&dir, "/join#first_one").unwrap();
        enqueue_hub_path(&dir, "/join#a.second_one").unwrap();
        let drained = drain_hub_paths(&dir);
        assert_eq!(drained, vec!["/join#first_one", "/join#a.second_one"]);
        assert!(drain_hub_paths(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn linux_desktop_entry_registers_both_schemes() {
        let home = std::env::temp_dir().join(format!("pbd-dl-linux-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        register(Os::Linux, &home, Path::new("/opt/Playbox/playbox-desktop")).unwrap();
        let s = std::fs::read_to_string(linux_desktop_entry_path(&home)).unwrap();
        assert!(s.contains("Exec=\"/opt/Playbox/playbox-desktop\" %u"));
        assert!(s.contains("MimeType=x-scheme-handler/playbox;x-scheme-handler/web+playbox;"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn linux_desktop_entry_escapes_specials_and_strips_newlines() {
        let s = render_linux_desktop_entry(Path::new("/opt/Play%box\nEvil=1/app\"$`\\bin"));
        assert!(!s.contains("\nEvil=1"));
        assert!(s.contains("Exec=\"/opt/Play%%boxEvil=1/app\\\"\\$\\`\\\\bin\" %u"));
    }
}
