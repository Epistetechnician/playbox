//! Native desktop menu bar (`Playbox`, `Edit`, `View`, `Window`).
//!
//! Essential on macOS so `⌘C` / `⌘V` / `⌘A` / `⌘Z` work inside `WKWebView` inputs, and gives every platform `⌘K` (Jump to Room),
//! `⌘,` (Settings), and `⌘[` / `⌘]` (Back / Forward) since the window has no browser address bar.

use crate::{
    quit, service,
    window::{self, UiAction},
};
use tauri::{
    menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, Wry,
};

pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let settings = MenuItem::with_id(app, "app:settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let quit_item = MenuItem::with_id(app, "app:quit", "Quit Playbox", true, Some("CmdOrCtrl+Q"))?;

    let app_menu = Submenu::with_items(
        app,
        "Playbox",
        true,
        &[
            &PredefinedMenuItem::about(
                app,
                Some("About Playbox"),
                Some(AboutMetadata {
                    name: Some("Playbox".into()),
                    version: Some(crate::update::CURRENT.into()),
                    ..Default::default()
                }),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;

    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    let jump = MenuItem::with_id(
        app,
        "app:palette",
        "Jump to Room…",
        true,
        Some("CmdOrCtrl+K"),
    )?;
    let all_rooms = MenuItem::with_id(app, "app:rooms", "All Rooms", true, Some("CmdOrCtrl+1"))?;
    let attention =
        MenuItem::with_id(app, "app:attention", "Attention", true, Some("CmdOrCtrl+2"))?;
    let back = MenuItem::with_id(app, "app:back", "Back", true, Some("CmdOrCtrl+["))?;
    let forward = MenuItem::with_id(app, "app:forward", "Forward", true, Some("CmdOrCtrl+]"))?;
    let reload = MenuItem::with_id(app, "app:reload", "Reload", true, Some("CmdOrCtrl+R"))?;

    let view_menu = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &jump,
            &all_rooms,
            &attention,
            &PredefinedMenuItem::separator(app)?,
            &back,
            &forward,
            &reload,
        ],
    )?;

    let window_menu = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    Menu::with_items(app, &[&app_menu, &edit_menu, &view_menu, &window_menu])
}

pub fn handle(app: &AppHandle, id: &str) {
    match id {
        "app:settings" => window::run_action(app, UiAction::Settings),
        "app:palette" => window::run_action(app, UiAction::Palette),
        "app:rooms" => window::navigate_path(app, service::port(), "/"),
        "app:attention" => window::navigate_path(app, service::port(), "/?view=attention"),
        "app:back" => window::run_action(app, UiAction::Back),
        "app:forward" => window::run_action(app, UiAction::Forward),
        "app:reload" => window::run_action(app, UiAction::Reload),
        "app:quit" => quit::graceful(app),
        _ => {}
    }
}
