//! The one window: a small local page while the hub starts, then the hub itself. The hub is a remote origin from the app's point of view
//! and has no IPC (see `capabilities/main.json`).

use crate::AppState;
use pbd_core::{
    identity::Product,
    signin,
    window_state::{WindowBounds, MIN_HEIGHT, MIN_WIDTH},
};
use std::path::Path;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const LABEL: &str = "main";

#[derive(Clone, Copy)]
pub enum UiAction {
    Palette,
    Settings,
    Back,
    Forward,
    Reload,
}

pub fn ensure(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(w) = app.get_webview_window(LABEL) {
        return Ok(w);
    }
    let bounds = WindowBounds::load(&app.state::<AppState>().dirs.data);
    let ua = format!(
        "Mozilla/5.0 PlayboxDesktop/{} ({})",
        crate::update::CURRENT,
        std::env::consts::OS
    );
    let mut b = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Playbox")
        .inner_size(bounds.width, bounds.height)
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
        .user_agent(&ua)
        .visible(false);
    #[cfg(target_os = "macos")]
    {
        b = b
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    if let (Some(x), Some(y)) = (bounds.x, bounds.y) {
        b = b.position(f64::from(x), f64::from(y));
    }
    let win = b.build()?;
    if bounds.maximized {
        let _ = win.maximize();
    }
    Ok(win)
}

/// Persist the window's logical bounds so the next launch opens right where the user left it.
pub fn save_bounds(win: &WebviewWindow, data_dir: &Path) {
    if win.is_minimized().unwrap_or(false) {
        return;
    }
    let scale = win.scale_factor().unwrap_or(1.0).max(0.5);
    let Ok(size) = win.inner_size() else {
        return;
    };
    let pos = win.outer_position().ok();
    let maximized = win.is_maximized().unwrap_or(false);
    let bounds = WindowBounds {
        x: pos.map(|p| (f64::from(p.x) / scale).round() as i32),
        y: pos.map(|p| (f64::from(p.y) / scale).round() as i32),
        width: (f64::from(size.width) / scale).round(),
        height: (f64::from(size.height) / scale).round(),
        maximized,
    };
    let _ = bounds.save(data_dir);
}

fn is_on_hub(win: &WebviewWindow, hub: &tauri::Url) -> bool {
    win.url()
        .map(|u| u.host_str() == hub.host_str() && u.port() == hub.port())
        .unwrap_or(false)
}

/// Show the hub in the window (once), keeping whatever the user is already looking at if it is already the hub.
pub fn show_hub(app: &AppHandle, port: u16) {
    let Ok(win) = ensure(app) else { return };
    let Ok(url) = Product::hub_url(port).parse::<tauri::Url>() else {
        return;
    };
    if !is_on_hub(&win, &url) {
        go(app, &win, port, url, None);
    }
}

/// Sign the window in again (a session expired, or the hub was reset) and show the hub.
pub fn sign_in(app: &AppHandle, port: u16) {
    let Ok(win) = ensure(app) else { return };
    if let Ok(url) = Product::hub_url(port).parse::<tauri::Url>() {
        go(app, &win, port, url, None);
    }
}

/// Reveal the window and navigate to a specific hub path (`/r/<id>/canvas`, `/?view=attention`, `/join#...`), signing in first if needed.
pub fn navigate_path(app: &AppHandle, port: u16, path: &str) {
    if !path.starts_with('/') {
        return;
    }
    reveal(app);
    let Ok(win) = ensure(app) else { return };
    let Ok(hub_root) = Product::hub_url(port).parse::<tauri::Url>() else {
        return;
    };
    let full = format!("http://localhost:{port}{path}");
    let Ok(dest) = full.parse::<tauri::Url>() else {
        return;
    };
    if is_on_hub(&win, &hub_root) {
        let _ = win.navigate(dest);
    } else {
        go(app, &win, port, dest, Some(path));
    }
}

/// Open the hub in the window, signing in on the way when the hub supports it. Falls back to the plain hub page.
fn go(app: &AppHandle, win: &WebviewWindow, port: u16, fallback: tauri::Url, next: Option<&str>) {
    let target = if signin::supported(port) {
        signin::prepare(&app.state::<AppState>().dirs.hub_home)
            .ok()
            .and_then(|n| {
                signin::url_with_next(port, &n, next)
                    .parse::<tauri::Url>()
                    .ok()
            })
    } else {
        None
    };
    let _ = win.navigate(target.unwrap_or(fallback));
}

pub fn run_action(app: &AppHandle, action: UiAction) {
    reveal(app);
    let Some(win) = app.get_webview_window(LABEL) else {
        return;
    };
    let script = match action {
        UiAction::Palette => "if(window.PBShell&&PBShell.openPalette){PBShell.openPalette();}else if(typeof openPalette==='function'){openPalette();}",
        UiAction::Settings => "if(window.PBTheme&&PBTheme.open){PBTheme.open();}",
        UiAction::Back => "history.back();",
        UiAction::Forward => "history.forward();",
        UiAction::Reload => "location.reload();",
    };
    let _ = win.eval(script);
}

pub fn set_badge(app: &AppHandle, count: usize) {
    if let Some(win) = app.get_webview_window(LABEL) {
        let badge = if count > 0 {
            i64::try_from(count).ok()
        } else {
            None
        };
        let _ = win.set_badge_count(badge);
    }
}

pub fn is_focused_and_visible(app: &AppHandle) -> bool {
    app.get_webview_window(LABEL)
        .map(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
        .unwrap_or(false)
}

/// Say something on the local page (only text we wrote ourselves is ever passed here).
pub fn say(app: &AppHandle, text: &str) {
    let Some(win) = app.get_webview_window(LABEL) else {
        return;
    };
    let esc = text
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace(['\n', '\r'], " ");
    let _ = win.eval(format!(
        "var s=document.getElementById('s');if(s)s.textContent='{esc}';"
    ));
}

pub fn reveal(app: &AppHandle) {
    if let Ok(w) = ensure(app) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
