//! How to run the hub as a supervised service: which Python, which script, which port and home, and what "healthy" means.

use crate::layout::Dirs;
use crate::probe::Probe;
use crate::supervisor::{Policy, Spec};
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    s.env = vec![
        ("PYTHONUNBUFFERED".into(), "1".into()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
    ];
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
}
