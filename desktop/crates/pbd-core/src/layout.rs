//! Where the app keeps things, per operating system, resolved from an injected environment so every OS can be tested on any machine.
//!
//! The hub's own home is **not** one of these directories: it keeps its passkeys, rooms and invites in `~/.playbox-hub` (or
//! `PLAYBOX_HUB_HOME`), and the desktop app uses exactly that, so an existing install carries over untouched.

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// The user's home directory.
    pub home: PathBuf,
    /// The app's own data (settings, claim lock).
    pub data: PathBuf,
    pub logs: PathBuf,
    /// The hub's home: shared with an existing install.
    pub hub_home: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub struct NoHome;

impl std::fmt::Display for NoHome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot tell where your home directory is (HOME or the Windows profile variables are not set)")
    }
}

impl std::error::Error for NoHome {}

pub fn resolve(os: Os, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Dirs, NoHome> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = get("HOME").or_else(|| get("USERPROFILE")).ok_or(NoHome)?;
    let (default_data, logs) = match os {
        Os::Mac => (
            home.join("Library/Application Support/Playbox"),
            home.join("Library/Logs/Playbox"),
        ),
        Os::Linux => (
            get("XDG_DATA_HOME")
                .unwrap_or_else(|| home.join(".local/share"))
                .join("playbox"),
            get("XDG_STATE_HOME")
                .unwrap_or_else(|| home.join(".local/state"))
                .join("playbox/logs"),
        ),
        Os::Windows => (
            get("APPDATA")
                .unwrap_or_else(|| home.join("AppData/Roaming"))
                .join("Playbox"),
            get("LOCALAPPDATA")
                .unwrap_or_else(|| home.join("AppData/Local"))
                .join("Playbox/Logs"),
        ),
    };
    let data = get("PLAYBOX_DESKTOP_DATA_DIR").unwrap_or(default_data);
    let hub_home = get("PLAYBOX_HUB_HOME").unwrap_or_else(|| home.join(".playbox-hub"));
    Ok(Dirs {
        home,
        data,
        logs,
        hub_home,
    })
}

pub fn from_process_env() -> Result<Dirs, NoHome> {
    resolve(Os::current(), &|k| std::env::var_os(k))
}

/// Where a packaged app keeps its resources, found from the running executable on our own instead of through the GUI toolkit. Tauri
/// refuses the executable path on macOS when any folder above it is a symlink (`/tmp`, a symlinked home or volume), and then reports no
/// resource directory at all, which made the app fall back to a system Python and never start its hub.
pub fn resources_for_exe(exe: &std::path::Path) -> Option<PathBuf> {
    let exe = exe.canonicalize().ok()?;
    let dir = exe.parent()?;
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["../Resources"]
    } else if cfg!(windows) {
        &["."]
    } else {
        // AppImage and .deb both put resources under <prefix>/lib/<productName>.
        &["../lib/Playbox", "../lib/playbox-desktop"]
    };
    candidates
        .iter()
        .filter_map(|c| dir.join(c).canonicalize().ok())
        .find(|p| p.is_dir())
}

/// What to register for launch-at-login and to relaunch: inside an AppImage the running executable lives in a temporary mount, so the
/// path that survives is the AppImage file itself (`APPIMAGE`).
pub fn launcher(exe: PathBuf, appimage: Option<OsString>) -> PathBuf {
    appimage
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
        .unwrap_or(exe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let m: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn mac_uses_library_and_keeps_the_existing_hub_home() {
        let d = resolve(Os::Mac, &env(&[("HOME", "/Users/a")])).unwrap();
        assert_eq!(
            d.data,
            PathBuf::from("/Users/a/Library/Application Support/Playbox")
        );
        assert_eq!(d.logs, PathBuf::from("/Users/a/Library/Logs/Playbox"));
        assert_eq!(d.hub_home, PathBuf::from("/Users/a/.playbox-hub"));
    }

    #[test]
    fn linux_follows_xdg_when_set_and_falls_back_when_not() {
        let d = resolve(Os::Linux, &env(&[("HOME", "/home/a")])).unwrap();
        assert_eq!(d.data, PathBuf::from("/home/a/.local/share/playbox"));
        let x = resolve(
            Os::Linux,
            &env(&[
                ("HOME", "/home/a"),
                ("XDG_DATA_HOME", "/data"),
                ("XDG_STATE_HOME", "/state"),
            ]),
        )
        .unwrap();
        assert_eq!(x.data, PathBuf::from("/data/playbox"));
        assert_eq!(x.logs, PathBuf::from("/state/playbox/logs"));
    }

    #[test]
    fn windows_uses_appdata() {
        let d = resolve(
            Os::Windows,
            &env(&[
                ("USERPROFILE", "C:/Users/a"),
                ("APPDATA", "C:/Users/a/AppData/Roaming"),
            ]),
        )
        .unwrap();
        assert_eq!(d.data, PathBuf::from("C:/Users/a/AppData/Roaming/Playbox"));
        assert_eq!(
            d.logs,
            PathBuf::from("C:/Users/a/AppData/Local/Playbox/Logs")
        );
    }

    #[test]
    fn an_explicit_hub_home_wins() {
        let d = resolve(
            Os::Mac,
            &env(&[("HOME", "/Users/a"), ("PLAYBOX_HUB_HOME", "/srv/hub")]),
        )
        .unwrap();
        assert_eq!(d.hub_home, PathBuf::from("/srv/hub"));
    }

    #[test]
    fn no_home_is_an_error_not_a_panic() {
        assert_eq!(resolve(Os::Linux, &env(&[])), Err(NoHome));
        assert_eq!(resolve(Os::Linux, &env(&[("HOME", "")])), Err(NoHome));
    }
}

#[cfg(test)]
mod launcher_tests {
    use super::*;

    #[test]
    fn an_appimage_registers_itself_not_its_temporary_mount() {
        let d = std::env::temp_dir().join(format!("pbd-launcher-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let img = d.join("Playbox.AppImage");
        std::fs::write(&img, b"x").unwrap();
        let exe = PathBuf::from("/tmp/.mount_abc/usr/bin/playbox-desktop");
        assert_eq!(launcher(exe.clone(), Some(img.clone().into())), img);
        assert_eq!(launcher(exe.clone(), None), exe);
        assert_eq!(launcher(exe.clone(), Some("relative".into())), exe);
        assert_eq!(launcher(exe.clone(), Some(d.join("gone").into())), exe);
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod resource_tests {
    use super::*;

    #[test]
    fn found_even_when_the_app_is_reached_through_a_symlinked_folder() {
        let root = std::env::temp_dir().join(format!("pbd-res-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let real = root.join("real/Playbox.app/Contents");
        std::fs::create_dir_all(real.join("MacOS")).unwrap();
        std::fs::create_dir_all(real.join("Resources")).unwrap();
        std::fs::write(real.join("MacOS/playbox-desktop"), b"").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let via_link = root.join("link/Playbox.app/Contents/MacOS/playbox-desktop");
        let got = resources_for_exe(&via_link).unwrap();
        assert!(got.ends_with("Contents/Resources"));
        assert!(resources_for_exe(&root.join("nope")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
