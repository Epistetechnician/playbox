//! The tray (menu-bar) item: room-aware status, urgent attention items, rooms list, quick actions, autostart, updates, logs and quit.

use crate::{
    quit, service,
    window::{self, UiAction},
    AppState,
};
use pbd_core::{autostart, layout::Os, status::Line, watch::HubSnapshot};
use std::sync::Mutex;
use tauri::{
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder},
    AppHandle, Manager, Wry,
};

struct TrayModel {
    base_status: String,
    update_text: String,
    update_enabled: bool,
    snapshot: HubSnapshot,
}

pub struct Handles {
    tray: TrayIcon<Wry>,
    model: Mutex<TrayModel>,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Open,
    Palette,
    Settings,
    OpenBrowser,
    SignIn,
    Update,
    Rollback,
    Restart,
    ToggleLogin,
    OpenLogs,
    Quit,
    Room(String),
    Attention(usize),
}

fn parse(id: &str) -> Option<Command> {
    if let Some(rid) = id.strip_prefix("room:") {
        if !rid.is_empty()
            && rid.len() <= 64
            && rid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Some(Command::Room(rid.to_string()));
        }
        return None;
    }
    if let Some(idx) = id.strip_prefix("attn:") {
        return idx.parse::<usize>().ok().map(Command::Attention);
    }
    Some(match id {
        "open" => Command::Open,
        "palette" => Command::Palette,
        "settings" => Command::Settings,
        "browser" => Command::OpenBrowser,
        "signin" => Command::SignIn,
        "update" => Command::Update,
        "rollback" => Command::Rollback,
        "restart" => Command::Restart,
        "login" => Command::ToggleLogin,
        "logs" => Command::OpenLogs,
        "quit" => Command::Quit,
        _ => return None,
    })
}

fn render_menu(app: &AppHandle, m: &TrayModel) -> tauri::Result<Menu<Wry>> {
    let state = app.state::<AppState>();
    let on = autostart::is_enabled(Os::current(), &state.dirs.home);
    let summary = m.snapshot.format_status(&m.base_status);

    let mut owned: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
    owned.push(Box::new(MenuItem::with_id(
        app,
        "status",
        summary,
        false,
        None::<&str>,
    )?));

    if !m.snapshot.attention.is_empty() {
        owned.push(Box::new(PredefinedMenuItem::separator(app)?));
        for (idx, item) in m.snapshot.attention.iter().take(4).enumerate() {
            owned.push(Box::new(MenuItem::with_id(
                app,
                format!("attn:{idx}"),
                item.menu_label(),
                true,
                None::<&str>,
            )?));
        }
    }

    if !m.snapshot.rooms.is_empty() {
        owned.push(Box::new(PredefinedMenuItem::separator(app)?));
        for room in m.snapshot.rooms.iter().take(8) {
            owned.push(Box::new(MenuItem::with_id(
                app,
                format!("room:{}", room.id),
                room.menu_label(),
                true,
                None::<&str>,
            )?));
        }
    }

    owned.push(Box::new(PredefinedMenuItem::separator(app)?));
    owned.push(Box::new(MenuItem::with_id(
        app,
        "open",
        "Open Playbox",
        true,
        None::<&str>,
    )?));
    owned.push(Box::new(MenuItem::with_id(
        app,
        "palette",
        "Jump to Room…",
        true,
        Some("CmdOrCtrl+K"),
    )?));
    owned.push(Box::new(MenuItem::with_id(
        app,
        "settings",
        "Settings…",
        true,
        Some("CmdOrCtrl+,"),
    )?));
    owned.push(Box::new(MenuItem::with_id(
        app,
        "browser",
        "Open in browser (passkeys)",
        true,
        None::<&str>,
    )?));
    owned.push(Box::new(MenuItem::with_id(
        app,
        "signin",
        "Sign in again",
        true,
        None::<&str>,
    )?));

    owned.push(Box::new(PredefinedMenuItem::separator(app)?));
    owned.push(Box::new(MenuItem::with_id(
        app,
        "restart",
        "Restart hub",
        true,
        None::<&str>,
    )?));
    owned.push(Box::new(CheckMenuItem::with_id(
        app,
        "login",
        "Launch at login",
        true,
        on,
        None::<&str>,
    )?));
    owned.push(Box::new(MenuItem::with_id(
        app,
        "update",
        &m.update_text,
        m.update_enabled,
        None::<&str>,
    )?));
    if crate::update::has_previous() {
        owned.push(Box::new(MenuItem::with_id(
            app,
            "rollback",
            "Roll back to previous version",
            true,
            None::<&str>,
        )?));
    }
    owned.push(Box::new(MenuItem::with_id(
        app,
        "logs",
        "Show logs",
        true,
        None::<&str>,
    )?));

    owned.push(Box::new(PredefinedMenuItem::separator(app)?));
    owned.push(Box::new(MenuItem::with_id(
        app,
        "quit",
        "Quit Playbox",
        true,
        Some("CmdOrCtrl+Q"),
    )?));

    let refs: Vec<&dyn IsMenuItem<Wry>> = owned.iter().map(AsRef::as_ref).collect();
    Menu::with_items(app, &refs)
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let initial = TrayModel {
        base_status: "Hub starting…".into(),
        update_text: "Check for updates".into(),
        update_enabled: true,
        snapshot: HubSnapshot::default(),
    };
    let menu = render_menu(app, &initial)?;
    let mut b = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .tooltip("Playbox")
        .show_menu_on_left_click(true);
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    let tray = b
        .on_menu_event(|app, ev| dispatch(app, ev.id().as_ref()))
        .build(app)?;
    app.manage(Handles {
        tray,
        model: Mutex::new(initial),
    });
    Ok(())
}

fn dispatch(app: &AppHandle, id: &str) {
    match parse(id) {
        Some(Command::Open) => window::reveal(app),
        Some(Command::Palette) => window::run_action(app, UiAction::Palette),
        Some(Command::Settings) => window::run_action(app, UiAction::Settings),
        Some(Command::Update) => crate::update::on_click(app),
        Some(Command::Rollback) => crate::update::on_rollback(app),
        Some(Command::SignIn) => {
            window::reveal(app);
            window::sign_in(app, service::port());
        }
        Some(Command::OpenBrowser) => open_browser(),
        Some(Command::Room(rid)) => {
            window::navigate_path(app, service::port(), &format!("/r/{rid}/canvas"));
        }
        Some(Command::Attention(idx)) => {
            let path = app
                .try_state::<Handles>()
                .and_then(|h| {
                    h.model
                        .lock()
                        .ok()
                        .and_then(|m| m.snapshot.attention.get(idx).map(|a| a.target_path()))
                })
                .unwrap_or_else(|| "/?view=attention".to_string());
            window::navigate_path(app, service::port(), &path);
        }
        Some(Command::Restart) => {
            if let Ok(g) = app.state::<AppState>().supervisor.lock() {
                if let Some(s) = g.as_ref() {
                    s.reset(); // closes an open circuit too
                    s.restart();
                }
            }
        }
        Some(Command::ToggleLogin) => {
            toggle_login(app);
            rebuild(app);
        }
        Some(Command::OpenLogs) => open_logs(app),
        Some(Command::Quit) => quit::graceful(app),
        None => {}
    }
}

fn rebuild(app: &AppHandle) {
    let Some(h) = app.try_state::<Handles>() else {
        return;
    };
    let (menu_res, badge) = {
        let Ok(m) = h.model.lock() else { return };
        (render_menu(app, &m), m.snapshot.badge_count())
    };
    if let Ok(menu) = menu_res {
        let _ = h.tray.set_menu(Some(menu));
    }
    #[cfg(target_os = "macos")]
    {
        let title = if badge > 0 {
            Some(format!(" {badge}"))
        } else {
            None
        };
        let _ = h.tray.set_title(title);
    }
    window::set_badge(app, badge);
}

pub fn set_status(app: &AppHandle, line: &Line) {
    let changed = app
        .try_state::<Handles>()
        .and_then(|h| {
            let mut m = h.model.lock().ok()?;
            if m.base_status == line.text {
                return Some(false);
            }
            m.base_status = line.text.clone();
            Some(true)
        })
        .unwrap_or(false);
    if changed {
        rebuild(app);
    }
}

pub fn set_update(app: &AppHandle, text: &str, enabled: bool) {
    let changed = app
        .try_state::<Handles>()
        .and_then(|h| {
            let mut m = h.model.lock().ok()?;
            if m.update_text == text && m.update_enabled == enabled {
                return Some(false);
            }
            m.update_text = text.to_string();
            m.update_enabled = enabled;
            Some(true)
        })
        .unwrap_or(false);
    if changed {
        rebuild(app);
    }
}

pub fn set_snapshot(app: &AppHandle, snap: HubSnapshot) {
    let changed = app
        .try_state::<Handles>()
        .and_then(|h| {
            let mut m = h.model.lock().ok()?;
            if m.snapshot == snap {
                return Some(false);
            }
            m.snapshot = snap;
            Some(true)
        })
        .unwrap_or(false);
    if changed {
        rebuild(app);
    }
}

fn toggle_login(app: &AppHandle) {
    let st = app.state::<AppState>();
    let os = Os::current();
    let _ = if autostart::is_enabled(os, &st.dirs.home) {
        autostart::disable(os, &st.dirs.home)
    } else {
        std::env::current_exe().and_then(|exe| {
            let exe = pbd_core::layout::launcher(exe, std::env::var_os("APPIMAGE"));
            autostart::enable(os, &st.dirs.home, &exe)
        })
    };
}

/// Passkeys need a real browser: the embedded webview has no platform authenticator for `localhost` (see docs/desktop-plan.md).
fn open_browser() {
    let url = pbd_core::identity::Product::hub_url(service::port());
    let _ = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&url)
            .spawn()
    } else {
        std::process::Command::new(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        })
        .arg(&url)
        .spawn()
    };
}

fn open_logs(app: &AppHandle) {
    let dir = &app.state::<AppState>().dirs.logs;
    let _ = std::fs::create_dir_all(dir);
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(dir).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_commands_and_unknown_ids_do_nothing() {
        assert_eq!(parse("restart"), Some(Command::Restart));
        assert_eq!(parse("quit"), Some(Command::Quit));
        assert_eq!(parse("browser"), Some(Command::OpenBrowser));
        assert_eq!(parse("palette"), Some(Command::Palette));
        assert_eq!(parse("settings"), Some(Command::Settings));
        assert_eq!(
            parse("room:gpu-lab_1"),
            Some(Command::Room("gpu-lab_1".into()))
        );
        assert_eq!(parse("room:../bad"), None);
        assert_eq!(parse("attn:2"), Some(Command::Attention(2)));
        assert_eq!(parse("status"), None);
        assert_eq!(parse("../etc/passwd"), None);
    }
}
