//! Run the hub in the background and start it at login: a launchd agent on macOS, a systemd user unit on Linux.
//! Files are written under the user's own directories (no root), an existing service that differs is backed up
//! and only replaced on a yes, and `install` doesn't return until the hub answers on its port.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What to run.
pub struct Spec {
    /// The program and its arguments, absolute paths.
    pub argv: Vec<String>,
    /// The hub's environment: PATH (so it finds `playbox`, `playbox-mcp`, `cloudflared`) and any `PLAYBOX_*` settings in force now.
    pub env: Vec<(String, String)>,
    /// Where stdout and stderr go.
    pub log: PathBuf,
    /// The hub's port, for the health check.
    pub port: u16,
}

pub fn label() -> String {
    std::env::var("PLAYBOX_SERVICE_LABEL")
        .ok()
        .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
        .unwrap_or_else(|| "com.playbox.hub".to_string())
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn macos() -> bool {
    cfg!(target_os = "macos")
}

/// The service definition's file.
pub fn unit_path() -> PathBuf {
    if macos() {
        home_dir().join("Library/LaunchAgents").join(format!("{}.plist", label()))
    } else {
        home_dir().join(".config/systemd/user").join(format!("{}.service", label()))
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn launchd_plist(label: &str, s: &Spec) -> String {
    let args: String = s.argv.iter().map(|a| format!("    <string>{}</string>\n", xml(a))).collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n  <key>Label</key><string>{}</string>\n  <key>ProgramArguments</key><array>\n{}  </array>\n\
  <key>EnvironmentVariables</key><dict>{}</dict>\n\
  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n  <key>ThrottleInterval</key><integer>10</integer>\n\
  <key>StandardOutPath</key><string>{}</string>\n  <key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
        xml(label),
        args,
        s.env.iter().map(|(k, v)| format!("<key>{}</key><string>{}</string>", xml(k), xml(v))).collect::<String>(),
        xml(&s.log.to_string_lossy()),
        xml(&s.log.to_string_lossy())
    )
}

/// One systemd command-line word: quoted, with `\`, `"` and `%` escaped.
fn sd_word(a: &str) -> String {
    format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"))
}

pub fn systemd_unit(s: &Spec) -> String {
    let exec: Vec<String> = s.argv.iter().map(|a| sd_word(a)).collect();
    format!(
        "[Unit]\nDescription=Playbox hub\nAfter=network-online.target\n\n[Service]\nExecStart={}\n{}\n\
Restart=on-failure\nRestartSec=10\nStandardOutput=append:{}\nStandardError=append:{}\n\n[Install]\nWantedBy=default.target\n",
        exec.join(" "),
        s.env.iter().map(|(k, v)| format!("Environment={}", sd_word(&format!("{k}={v}")))).collect::<Vec<_>>().join("\n"),
        s.log.display(),
        s.log.display()
    )
}

fn uid() -> Result<String> {
    let o = Command::new("id").arg("-u").output().context("could not run `id -u`")?;
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn sh(prog: &str, args: &[&str]) -> Result<String> {
    let o = Command::new(prog).args(args).stdin(Stdio::null()).output().with_context(|| format!("could not run {prog}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if !o.status.success() {
        bail!("{prog} {} failed: {}", args.join(" "), text.trim());
    }
    Ok(text)
}

fn definition(s: &Spec) -> String {
    if macos() {
        launchd_plist(&label(), s)
    } else {
        systemd_unit(s)
    }
}

/// Whether the installed definition is exactly what `s` would write.
pub fn up_to_date(s: &Spec) -> bool {
    std::fs::read_to_string(unit_path()).is_ok_and(|t| t == definition(s))
}

pub fn installed() -> bool {
    unit_path().is_file()
}

fn load(path: &Path) -> Result<()> {
    if macos() {
        let target = format!("gui/{}", uid()?);
        let _ = sh("launchctl", &["bootout", &format!("{target}/{}", label())]); // fine if it wasn't loaded
        std::thread::sleep(Duration::from_secs(2)); // bootstrap right after bootout can fail with an I/O error
        sh("launchctl", &["bootstrap", &target, &path.to_string_lossy()])?;
    } else {
        sh("systemctl", &["--user", "daemon-reload"])?;
        sh("systemctl", &["--user", "enable", "--now", &format!("{}.service", label())])?;
        sh("systemctl", &["--user", "restart", &format!("{}.service", label())])?;
    }
    Ok(())
}

/// Where the service manager has this label loaded from, if it is loaded at all.
fn loaded_from() -> Option<PathBuf> {
    if macos() {
        let t = sh("launchctl", &["print", &format!("gui/{}/{}", uid().ok()?, label())]).ok()?;
        t.lines().find_map(|l| l.trim().strip_prefix("path = ")).map(PathBuf::from)
    } else {
        let t = sh("systemctl", &["--user", "show", "-p", "FragmentPath", "--value", &format!("{}.service", label())]).ok()?;
        let p = t.trim();
        (!p.is_empty()).then(|| PathBuf::from(p))
    }
}

/// Write the definition (backing up a different one), load it, and wait for the hub to answer. The caller has
/// already decided that replacing an existing, different definition is fine.
pub fn install(s: &Spec) -> Result<PathBuf> {
    let path = unit_path();
    // Never take over a service loaded from somewhere else (another HOME, another install): booting it out by label would stop it.
    if let Some(other) = loaded_from() {
        let same = std::fs::canonicalize(&other).ok() == std::fs::canonicalize(&path).ok() || other == path;
        if !same {
            bail!("a service named {} is already loaded from {}; not touching it (set PLAYBOX_SERVICE_LABEL to use another name)", label(), other.display());
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = s.log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.is_file() && !up_to_date(s) {
        let bak = path.with_extension(format!("{}.bak", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
        std::fs::copy(&path, &bak).with_context(|| format!("could not back up {}", path.display()))?;
    }
    std::fs::write(&path, definition(s))?;
    load(&path)?;
    if !wait_healthy(s.port, Duration::from_secs(30)) {
        bail!("the hub didn't answer on port {} within 30 s; see {}", s.port, s.log.display());
    }
    Ok(path)
}

pub fn uninstall() -> Result<bool> {
    let path = unit_path();
    if !path.is_file() {
        return Ok(false);
    }
    if macos() {
        let _ = sh("launchctl", &["bootout", &format!("gui/{}/{}", uid()?, label())]);
    } else {
        let _ = sh("systemctl", &["--user", "disable", "--now", &format!("{}.service", label())]);
    }
    std::fs::remove_file(&path)?;
    if !macos() {
        let _ = sh("systemctl", &["--user", "daemon-reload"]);
    }
    Ok(true)
}

/// (running, one line of detail)
pub fn status() -> (bool, String) {
    if !installed() {
        return (false, "not installed".into());
    }
    let out = if macos() {
        sh("launchctl", &["print", &format!("gui/{}/{}", uid().unwrap_or_default(), label())])
    } else {
        sh("systemctl", &["--user", "is-active", &format!("{}.service", label())])
    };
    match out {
        Ok(t) if macos() => {
            let running = t.contains("state = running");
            (running, if running { "running".into() } else { "installed but not running".into() })
        }
        Ok(t) => (t.trim() == "active", t.trim().to_string()),
        Err(_) => (false, "installed but not running".into()),
    }
}

/// GET /api/auth/state on 127.0.0.1:port until it answers 200 or the time is up.
pub fn wait_healthy(port: u16, within: Duration) -> bool {
    let end = Instant::now() + within;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    while Instant::now() < end {
        if healthy(addr) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    false
}

fn healthy(addr: SocketAddr) -> bool {
    let Ok(mut c) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else { return false };
    let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
    if c.write_all(b"GET /api/auth/state HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return false;
    }
    let mut head = [0u8; 16];
    c.read(&mut head).is_ok_and(|n| head[..n].starts_with(b"HTTP/1.") && head[..n].windows(3).any(|w| w == b"200"))
}

/// The pairing links the hub printed at its last start, newest last (`/pair#<one-time code>`).
pub fn pairing_links(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in log.lines() {
        let t = line.trim();
        if let Some(i) = t.find("/pair#") {
            let start = t[..i].rfind("http").unwrap_or(0);
            let end = t[i..].find(char::is_whitespace).map_or(t.len(), |e| i + e);
            out.push(t[start..end].to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            argv: vec!["/usr/bin/python3".into(), "/opt/pb hub/ui/playbox_hub.py".into(), "--cloudflare-hostname".into(), "a&b.example.com".into()],
            env: vec![("PATH".into(), "/a:/b".into()), ("PLAYBOX_HOME".into(), "/data/pb".into())],
            log: PathBuf::from("/home/u/.playbox/hub.log"),
            port: 7717,
        }
    }

    #[test]
    fn the_launchd_file_is_valid_and_escaped() {
        let p = launchd_plist("com.playbox.hub", &spec());
        assert!(p.contains("<key>Label</key><string>com.playbox.hub</string>"));
        assert!(p.contains("<string>a&amp;b.example.com</string>"));
        assert!(p.contains("<string>/opt/pb hub/ui/playbox_hub.py</string>"));
        assert!(p.contains("<key>RunAtLoad</key><true/>") && p.contains("<key>KeepAlive</key><true/>"));
        assert!(p.contains("<key>PATH</key><string>/a:/b</string>") && p.contains("<key>PLAYBOX_HOME</key><string>/data/pb</string>"));
        assert!(p.contains("<key>StandardOutPath</key><string>/home/u/.playbox/hub.log</string>"));
        assert!(!p.contains("a&b"));
    }

    #[test]
    fn the_systemd_unit_quotes_every_word() {
        let mut s = spec();
        s.argv.push("100%".into());
        s.argv.push("say \"hi\"".into());
        let u = systemd_unit(&s);
        assert!(u.contains("ExecStart=\"/usr/bin/python3\" \"/opt/pb hub/ui/playbox_hub.py\""));
        assert!(u.contains("\"100%%\"") && u.contains("\"say \\\"hi\\\"\""));
        assert!(u.contains("Environment=\"PATH=/a:/b\"\nEnvironment=\"PLAYBOX_HOME=/data/pb\""));
        assert!(u.contains("Restart=on-failure") && u.contains("WantedBy=default.target"));
        assert!(u.contains("StandardOutput=append:/home/u/.playbox/hub.log"));
    }

    #[test]
    fn the_label_is_sanitised() {
        std::env::set_var("PLAYBOX_SERVICE_LABEL", "../evil;x");
        assert_eq!(label(), "com.playbox.hub");
        std::env::set_var("PLAYBOX_SERVICE_LABEL", "com.playbox.hubtest");
        assert_eq!(label(), "com.playbox.hubtest");
        std::env::remove_var("PLAYBOX_SERVICE_LABEL");
    }

    #[test]
    fn health_is_a_200_from_the_hub() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut buf = [0u8; 256];
                let _ = c.read(&mut buf);
                let _ = c.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\n{}");
            }
        });
        assert!(wait_healthy(port, Duration::from_secs(3)));
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(!wait_healthy(free, Duration::from_millis(600)));
    }

    #[test]
    fn pairing_links_are_read_from_the_log() {
        let log = "  Pair this device (valid 10 min, single use):\n    http://localhost:7717/pair#abc123\n    https://x.trycloudflare.com/pair#abc123      <- passkeys, from any device\nnoise\n";
        assert_eq!(pairing_links(log), ["http://localhost:7717/pair#abc123", "https://x.trycloudflare.com/pair#abc123"]);
        assert!(pairing_links("Sign in with your passkey at http://localhost:7717").is_empty());
    }
}
