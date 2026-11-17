//! The tray (menu-bar) item: status line, open, restart, launch at login, logs, quit.

use crate::{quit, window, AppState};
use pbd_core::{autostart, layout::Os, status::Line};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, Wry,
};

pub struct Handles {
    status: MenuItem<Wry>,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Open,
    OpenBrowser,
    Restart,
    ToggleLogin,
    OpenLogs,
    Quit,
}

fn parse(id: &str) -> Option<Command> {
    Some(match id {
        "open" => Command::Open,
        "browser" => Command::OpenBrowser,
        "restart" => Command::Restart,
        "login" => Command::ToggleLogin,
        "logs" => Command::OpenLogs,
        "quit" => Command::Quit,
        _ => return None,
    })
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<AppState>();
    let on = autostart::is_enabled(Os::current(), &state.dirs.home);
    let status = MenuItem::with_id(app, "status", "Hub starting…", false, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "open", "Open Playbox", true, None::<&str>)?,
            &MenuItem::with_id(
                app,
                "browser",
                "Open in browser (passkeys)",
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(app, "restart", "Restart hub", true, None::<&str>)?,
            &CheckMenuItem::with_id(app, "login", "Launch at login", true, on, None::<&str>)?,
            &MenuItem::with_id(app, "logs", "Show logs", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Quit Playbox", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    app.manage(Handles { status });
    let mut b = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .tooltip("Playbox")
        .show_menu_on_left_click(true);
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.on_menu_event(|app, ev| match parse(ev.id().as_ref()) {
        Some(Command::Open) => window::reveal(app),
        Some(Command::OpenBrowser) => open_browser(),
        Some(Command::Restart) => {
            if let Ok(g) = app.state::<AppState>().supervisor.lock() {
                if let Some(s) = g.as_ref() {
                    s.reset(); // closes an open circuit too
                    s.restart();
                }
            }
        }
        Some(Command::ToggleLogin) => toggle_login(app),
        Some(Command::OpenLogs) => open_logs(app),
        Some(Command::Quit) => quit::graceful(app),
        None => {}
    })
    .build(app)?;
    Ok(())
}

pub fn set_status(app: &AppHandle, line: &Line) {
    if let Some(h) = app.try_state::<Handles>() {
        let _ = h.status.set_text(&line.text);
    }
}

fn toggle_login(app: &AppHandle) {
    let st = app.state::<AppState>();
    let os = Os::current();
    let _ = if autostart::is_enabled(os, &st.dirs.home) {
        autostart::disable(os, &st.dirs.home)
    } else {
        std::env::current_exe().and_then(|exe| autostart::enable(os, &st.dirs.home, &exe))
    };
}

/// Passkeys need a real browser: the embedded webview has no platform authenticator for `localhost` (see docs/desktop-plan.md).
fn open_browser() {
    let url = pbd_core::identity::Product::hub_url(crate::service::port());
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
        assert_eq!(parse("status"), None);
        assert_eq!(parse("../etc/passwd"), None);
    }
}
