//! Updates: check for a newer signed build, download it, verify it, swap the app bundle and relaunch.
//!
//! The trust decisions live in `pbd_core::update` (signed checksum list, pinned key, hashes). This module only does what needs the
//! operating system: unpack, check the new bundle's code signature and the version it reports, swap with two renames, relaunch.
//! Never silent: the app checks on its own, but installing is always the owner's click in the tray menu.

use crate::{quit, tray, AppState};
use pbd_core::update::{self, Check, Release, Source, Version};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The release asset for this platform, if we publish one.
fn asset() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("Playbox-macos-arm64.zip")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("Playbox-macos-x86_64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("Playbox-linux-x86_64.AppImage")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("Playbox-linux-aarch64.AppImage")
    } else {
        None
    }
}

#[derive(Default)]
pub struct Updates {
    pending: Mutex<Option<Release>>,
    busy: Mutex<bool>,
}

/// What we replace when updating: the `.app` bundle on macOS, the AppImage file on Linux. `None` for a build that is neither (a
/// `cargo run` build, a `.deb` managed by the package manager), which is never replaced.
fn bundle() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        return std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_file());
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app") && p.join("Contents/MacOS").is_dir())
        .map(Path::to_path_buf)
}

/// `PLAYBOX_DESKTOP_UPDATE`: `off` (never look), `check` (default: look daily, install only on a click) or `install` (a managed host
/// that wants unattended updates). `PLAYBOX_DESKTOP_UPDATE_DELAY_SECS` moves the first look (default 60).
fn policy() -> String {
    std::env::var("PLAYBOX_DESKTOP_UPDATE").unwrap_or_else(|_| "check".into())
}

pub fn start_background_checks(app: &AppHandle) {
    if policy() == "off" {
        tray::set_update(app, "Updates: off on this machine", false);
        return;
    }
    if asset().is_none() || bundle().is_none() {
        tray::set_update(app, "Updates: from a packaged app only", false);
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let delay = std::env::var("PLAYBOX_DESKTOP_UPDATE_DELAY_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        std::thread::sleep(std::time::Duration::from_secs(delay));
        loop {
            check(&app, false);
            if policy() == "install" {
                apply(&app);
            }
            std::thread::sleep(std::time::Duration::from_secs(24 * 3600));
        }
    });
}

/// The tray item: install what was found, or look.
pub fn on_click(app: &AppHandle) {
    let ready = app
        .state::<Updates>()
        .pending
        .lock()
        .ok()
        .is_some_and(|p| p.is_some());
    let app = app.clone();
    std::thread::spawn(move || {
        if ready {
            apply(&app);
        } else {
            check(&app, true);
        }
    });
}

fn busy(app: &AppHandle, on: bool) -> bool {
    let u = app.state::<Updates>();
    let Ok(mut b) = u.busy.lock() else {
        return false;
    };
    if on && *b {
        return false;
    }
    *b = on;
    true
}

fn check(app: &AppHandle, said: bool) {
    let Some(asset) = asset() else { return };
    if !busy(app, true) {
        return;
    }
    if said {
        tray::set_update(app, "Checking for updates…", false);
    }
    let current = Version::parse(CURRENT).unwrap_or(Version(0, 0, 0));
    let result = update::check(
        &Source::github_latest(),
        current,
        asset,
        &update::pinned_keys(),
    );
    let updates = app.state::<Updates>();
    match result {
        Ok(Check::Available(rel)) => {
            tray::set_update(app, &format!("Install update {}…", rel.version), true);
            if let Ok(mut p) = updates.pending.lock() {
                *p = Some(rel);
            }
        }
        Ok(Check::UpToDate) => {
            tray::set_update(app, &format!("Up to date ({CURRENT}). Check again"), true)
        }
        Ok(Check::NoBuild) => {
            tray::set_update(app, "A newer release has no desktop build yet", true)
        }
        Err(e) => {
            // Quiet by default: a failed background check is not news. A click shows why.
            eprintln!("playbox-desktop: update check: {e}");
            tray::set_update(
                app,
                if said {
                    "Update check failed. Try again"
                } else {
                    "Check for updates"
                },
                true,
            );
        }
    }
    busy(app, false);
}

fn apply(app: &AppHandle) {
    let Some(rel) = app
        .state::<Updates>()
        .pending
        .lock()
        .ok()
        .and_then(|mut p| p.take())
    else {
        return;
    };
    if !busy(app, true) {
        return;
    }
    tray::set_update(app, &format!("Installing {}…", rel.version), false);
    match install(app, &rel) {
        Ok(()) => quit::graceful(app), // the relaunch script waits for us to exit
        Err(e) => {
            eprintln!("playbox-desktop: update to {} failed: {e}", rel.version);
            tray::set_update(app, "Update failed; nothing was changed. Check again", true);
            busy(app, false);
        }
    }
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let o = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn install(app: &AppHandle, rel: &Release) -> Result<(), String> {
    if cfg!(target_os = "linux") {
        return install_appimage(app, rel);
    }
    let bundle = bundle().ok_or("not running from an app bundle")?;
    let parent = bundle.parent().ok_or("no parent directory")?;
    let dirs = &app.state::<AppState>().dirs;

    // 1. The archive, checked against the signed hash.
    let dl = dirs.data.join("updates");
    let _ = std::fs::remove_dir_all(&dl);
    let zip = update::download(&Source::github_latest(), rel, &dl).map_err(|e| e.to_string())?;

    // 2. Unpack next to the installed app (same volume, so the swap is a rename). Refuse if we cannot write there.
    let stage = parent.join(format!(".Playbox-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir(&stage)
        .map_err(|e| format!("cannot write next to the app ({e}); move Playbox to Applications"))?;
    let result = (|| {
        run(Command::new("ditto")
            .args(["-x", "-k"])
            .arg(&zip)
            .arg(&stage))?;
        let new = stage.join("Playbox.app");
        // 3. The new bundle must be intact and must report the version the signed manifest promised.
        run(Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&new))?;
        let said = run(Command::new(new.join("Contents/MacOS/playbox-desktop")).arg("--version"))?;
        if said.trim() != format!("playbox-desktop {}", rel.version) {
            return Err(format!(
                "the new app reports {:?}, not {}",
                said.trim(),
                rel.version
            ));
        }
        // 4. Swap: the old app is kept as Playbox.previous.app (one level of rollback), then the new one takes its place.
        let previous = parent.join("Playbox.previous.app");
        let _ = std::fs::remove_dir_all(&previous);
        std::fs::rename(&bundle, &previous).map_err(|e| e.to_string())?;
        if let Err(e) = std::fs::rename(&new, &bundle) {
            let _ = std::fs::rename(&previous, &bundle); // put the old one back
            return Err(e.to_string());
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&stage);
    let _ = std::fs::remove_dir_all(&dl);
    result?;
    relaunch(&bundle);
    Ok(())
}

/// Linux: the release asset is the AppImage itself. Download (checked against the signed hash), make it runnable, ask it for its version,
/// then swap it in with two renames, keeping the old one as `<name>.previous`.
fn install_appimage(app: &AppHandle, rel: &Release) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let current = bundle().ok_or("not running from an AppImage")?;
    let parent = current.parent().ok_or("no parent directory")?;
    let dirs = &app.state::<AppState>().dirs;
    let dl = dirs.data.join("updates");
    let _ = std::fs::remove_dir_all(&dl);
    let image = update::download(&Source::github_latest(), rel, &dl).map_err(|e| e.to_string())?;

    // Same folder as the installed file, so the swap is a rename on one volume.
    let new = parent.join(format!(".Playbox-update-{}.AppImage", std::process::id()));
    let result = (|| {
        std::fs::copy(&image, &new).map_err(|e| format!("cannot write next to the app ({e})"))?;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        let said = run(Command::new(&new)
            .env("APPIMAGE_EXTRACT_AND_RUN", "1") // the check must work on a machine without FUSE too
            .arg("--version"))?;
        if said.trim() != format!("playbox-desktop {}", rel.version) {
            return Err(format!(
                "the new app reports {:?}, not {}",
                said.trim(),
                rel.version
            ));
        }
        let mut previous = current.clone().into_os_string();
        previous.push(".previous");
        let previous = PathBuf::from(previous);
        let _ = std::fs::remove_file(&previous);
        std::fs::rename(&current, &previous).map_err(|e| e.to_string())?;
        if let Err(e) = std::fs::rename(&new, &current) {
            let _ = std::fs::rename(&previous, &current); // put the old one back
            return Err(e.to_string());
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&new);
    let _ = std::fs::remove_dir_all(&dl);
    result?;
    relaunch(&current);
    Ok(())
}

/// Wait for this process to be gone (it holds the single-instance claim), then open the new app.
fn relaunch(bundle: &Path) {
    use std::os::unix::process::CommandExt;
    let _ = Command::new("sh")
        .args([
            "-c",
            "while kill -0 \"$1\" 2>/dev/null; do sleep 0.3; done; unset APPDIR ARGV0 OWD; if [ \"$(uname)\" = Darwin ]; then open \"$2\"; else exec \"$2\"; fi",
            "sh",
            &std::process::id().to_string(),
        ])
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}
