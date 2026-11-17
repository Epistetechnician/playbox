//! The one window: a small local page while the hub starts, then the hub itself. The hub is a remote origin from the app's point of view
//! and has no IPC (see `capabilities/main.json`).

use pbd_core::identity::Product;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const LABEL: &str = "main";

pub fn ensure(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(w) = app.get_webview_window(LABEL) {
        return Ok(w);
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Playbox")
        .inner_size(1280.0, 840.0)
        .min_inner_size(420.0, 520.0)
        .visible(false)
        .build()
}

/// Show the hub in the window (once), keeping whatever the user is already looking at if it is already the hub.
pub fn show_hub(app: &AppHandle, port: u16) {
    let Ok(win) = ensure(app) else { return };
    let Ok(url) = Product::hub_url(port).parse::<tauri::Url>() else {
        return;
    };
    let on_hub = win
        .url()
        .map(|u| u.host_str() == url.host_str() && u.port() == url.port())
        .unwrap_or(false);
    if !on_hub {
        let _ = win.navigate(url);
    }
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
