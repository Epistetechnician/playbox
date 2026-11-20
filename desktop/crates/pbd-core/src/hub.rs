//! How to run the hub as a supervised service: which Python, which script, which port and home, and what "healthy" means.

use crate::layout::Dirs;
use crate::probe::Probe;
use crate::supervisor::{Policy, Spec};
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

pub const NETWORK_FILE: &str = "desktop-network.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetworkMode {
    #[default]
    Local,
    Tailscale,
    Cloudflare,
}

impl NetworkMode {
    pub fn flag(self) -> Option<&'static str> {
        match self {
            Self::Local => None,
            Self::Tailscale => Some("--tailscale"),
            Self::Cloudflare => Some("--cloudflare"),
        }
    }
}

/// Read `<hub_home>/desktop-network.json` (`0600`, written by `Settings -> Hub -> Network`). Returns `(mode, stamp)` so the watcher
/// can detect changes and restart the supervised hub with the newly requested reach flag.
pub fn read_network_mode(hub_home: &Path) -> (NetworkMode, u64) {
    let path = hub_home.join(NETWORK_FILE);
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return (NetworkMode::Local, 0);
    };
    if !meta.file_type().is_file() {
        return (NetworkMode::Local, 0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let me = rustix::process::getuid().as_raw();
        if meta.uid() != me || (meta.permissions().mode() & 0o077) != 0 {
            return (NetworkMode::Local, 0);
        }
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(1, |d| d.as_secs());
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return (NetworkMode::Local, 0);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return (NetworkMode::Local, 0);
    };
    let mode = match v.get("mode").and_then(|x| x.as_str()) {
        Some("tailscale") => NetworkMode::Tailscale,
        Some("cloudflare") => NetworkMode::Cloudflare,
        _ => NetworkMode::Local,
    };
    let stamp = v
        .get("updated")
        .and_then(|x| x.as_u64())
        .unwrap_or(mtime)
        .max(1);
    (mode, stamp)
}

#[derive(Clone, Debug)]
pub struct HubConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    pub port: u16,
    pub extra_args: Vec<OsString>,
}

/// The app bundle's resources directory holds `python/` (a relocatable interpreter) and `hub/` (the hub's files). Without a bundled
/// interpreter (a development checkout) the system `python3` is used.
pub fn locate(resources: &Path, port: u16) -> HubConfig {
    let bundled = if cfg!(windows) {
        resources.join("python/python.exe")
    } else {
        resources.join("python/bin/python3")
    };
    let python = if bundled.is_file() {
        bundled
    } else {
        PathBuf::from(if cfg!(windows) { "python" } else { "python3" })
    };
    HubConfig {
        python,
        script: resources.join("hub/playbox_hub.py"),
        port,
        extra_args: vec![],
    }
}

/// The hub answers this without a login, so it can be probed from outside a session.
pub const HEALTH_PATH: &str = "/api/auth/state";

pub fn probe(port: u16) -> Probe {
    Probe::http(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        HEALTH_PATH,
        &[200],
    )
}

fn has_explicit_reach_flag(args: &[OsString]) -> bool {
    args.iter().any(|a| {
        let s = a.to_string_lossy();
        matches!(
            s.as_ref(),
            "--tailscale" | "--cloudflare" | "--cloudflare-tunnel" | "--lan"
        )
    })
}

pub fn spec(cfg: &HubConfig, dirs: &Dirs) -> Spec {
    let mut s = Spec::new("hub", &cfg.python);
    s.args = vec![
        cfg.script.clone().into_os_string(),
        "--port".into(),
        cfg.port.to_string().into(),
        "--home".into(),
        dirs.hub_home.clone().into_os_string(),
    ];
    s.args.extend(cfg.extra_args.iter().cloned());
    if !has_explicit_reach_flag(&cfg.extra_args) {
        let (mode, _) = read_network_mode(&dirs.hub_home);
        if let Some(flag) = mode.flag() {
            s.args.push(flag.into());
        }
    }
    s.env = vec![
        ("PYTHONUNBUFFERED".into(), "1".into()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("PYTHONNOUSERSITE".into(), "1".into()),
    ];
    // AppImage's AppRun injects PYTHONHOME=$APPDIR/usr/ and PYTHONPATH=$APPDIR/usr/share/pyshared/, which breaks the bundled
    // relocatable CPython (`No module named 'encodings'`) before the hub can bind its port.
    s.env_remove = vec!["PYTHONHOME".into(), "PYTHONPATH".into()];
    s.cwd = cfg.script.parent().map(Path::to_path_buf);
    s.log = Some(dirs.logs.join("hub.log"));
    s.health = Some(probe(cfg.port));
    s.attach = true; // an older install (a LaunchAgent, a terminal) may already be serving: watch it instead of fighting it for the port
    s.policy = Policy {
        start_timeout: Duration::from_secs(45),
        grace: Duration::from_secs(10),
        ..Policy::default()
    };
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> Dirs {
        Dirs {
            home: "/u".into(),
            data: "/d".into(),
            logs: "/l".into(),
            hub_home: "/h".into(),
        }
    }

    #[test]
    fn the_spec_uses_the_existing_hub_home_and_attaches() {
        let cfg = HubConfig {
            python: "/py".into(),
            script: "/res/hub/playbox_hub.py".into(),
            port: 7717,
            extra_args: vec!["--no-weak".into()],
        };
        let s = spec(&cfg, &dirs());
        assert_eq!(s.args[0], OsString::from("/res/hub/playbox_hub.py"));
        let a: Vec<String> = s
            .args
            .iter()
            .map(|x| x.to_string_lossy().into_owned())
            .collect();
        assert_eq!(a[1..5], ["--port", "7717", "--home", "/h"]);
        assert_eq!(a.last().map(String::as_str), Some("--no-weak"));
        assert!(s.attach);
        assert!(s.env_remove.contains(&OsString::from("PYTHONHOME")));
        assert!(s.env_remove.contains(&OsString::from("PYTHONPATH")));
        assert_eq!(s.log, Some(PathBuf::from("/l/hub.log")));
        assert_eq!(s.health.as_ref().map(|p| p.addr().port()), Some(7717));
    }

    #[test]
    fn it_falls_back_to_system_python_without_a_bundled_one() {
        let cfg = locate(Path::new("/definitely/not/here"), 7717);
        assert!(cfg.python.ends_with("python3") || cfg.python.ends_with("python"));
        assert_eq!(
            cfg.script,
            PathBuf::from("/definitely/not/here/hub/playbox_hub.py")
        );
    }

    #[test]
    fn the_hub_only_ever_listens_on_loopback() {
        assert!(probe(7717).addr().ip().is_loopback());
    }

    #[test]
    fn saved_network_mode_adds_reach_flag_unless_explicitly_overridden() {
        let h = std::env::temp_dir().join(format!("pbd-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        let p = h.join(NETWORK_FILE);
        std::fs::write(&p, r#"{"mode":"tailscale","updated":42}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(read_network_mode(&h), (NetworkMode::Tailscale, 42));

        let mut d = dirs();
        d.hub_home = h.clone();
        let cfg = HubConfig {
            python: "/py".into(),
            script: "/res/hub/playbox_hub.py".into(),
            port: 7717,
            extra_args: vec![],
        };
        let s = spec(&cfg, &d);
        assert!(s.args.iter().any(|a| a == "--tailscale"));

        // Explicit env/CLI tunnel flag takes precedence over the UI file.
        let cfg_explicit = HubConfig {
            extra_args: vec!["--cloudflare-tunnel".into(), "my-tun".into()],
            ..cfg
        };
        let s2 = spec(&cfg_explicit, &d);
        assert!(!s2.args.iter().any(|a| a == "--tailscale"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(read_network_mode(&h), (NetworkMode::Local, 0));
        }
        let _ = std::fs::remove_dir_all(&h);
    }
}
