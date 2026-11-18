//! Playbox Desktop: a thin native shell. All decisions live in `pbd-core`; this crate only connects them to a window, a menu bar,
//! a tray item and the operating system's lifecycle events.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod menu;
mod notify;
mod quit;
mod service;
mod tray;
mod update;
mod window;

use pbd_core::{
    claim::Claim,
    deeplink,
    layout::{self, Os},
    stepup,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{Manager, RunEvent};

/// What the running app owns. Dropping it releases the claim.
pub struct AppState {
    pub dirs: layout::Dirs,
    pub supervisor: Mutex<Option<pbd_core::supervisor::Supervisor>>,
    pub stepup: Mutex<Option<stepup::Bridge>>,
    pub stop_bg: Arc<AtomicBool>,
    pub pending_links: Mutex<Vec<String>>,
    pub hub_ready: AtomicBool,
    _claim: Claim,
}

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args.iter().any(|a| a == "--version") {
        println!("playbox-desktop {}", update::CURRENT);
        return;
    }
    let dirs = match layout::from_process_env() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("playbox-desktop: {e}");
            std::process::exit(1);
        }
    };
    let cli_links = deeplink::from_args(&raw_args);
    let claim = match Claim::acquire(&dirs.data) {
        Ok(c) => c,
        Err(e) => {
            // Another Playbox Desktop is already running for this user: hand it any invite deep links so it focuses and opens them.
            for link in &cli_links {
                let _ = deeplink::enqueue_hub_path(&dirs.data, link);
            }
            if cli_links.is_empty() {
                eprintln!("playbox-desktop: {e}");
            }
            std::process::exit(0);
        }
    };

    if let Ok(exe) = std::env::current_exe() {
        let exe = layout::launcher(exe, std::env::var_os("APPIMAGE"));
        let _ = deeplink::register(Os::current(), &dirs.home, &exe);
    }

    let background = raw_args.iter().any(|a| a == "--background") && cli_links.is_empty();
    let bridge = stepup::Bridge::start(dirs.hub_home.clone(), stepup::OsApprover);
    let state = AppState {
        dirs,
        supervisor: Mutex::new(None),
        stepup: Mutex::new(Some(bridge)),
        stop_bg: Arc::new(AtomicBool::new(false)),
        pending_links: Mutex::new(cli_links),
        hub_ready: AtomicBool::new(false),
        _claim: claim,
    };

    let app = tauri::Builder::default()
        .manage(state)
        .manage(update::Updates::default())
        .menu(menu::build)
        .on_menu_event(|app, ev| menu::handle(app, ev.id().as_ref()))
        .setup(move |app| {
            quit::on_signals(app.handle());
            tray::build(app.handle())?;
            let win = window::ensure(app.handle())?;
            if !background {
                let _ = win.show();
            }
            update::start_background_checks(app.handle());
            let sup = service::start(app.handle());
            if let Ok(mut g) = app.state::<AppState>().supervisor.lock() {
                *g = Some(sup);
            }
            service::start_watcher(app.handle());
            Ok(())
        })
        .on_window_event(|w, e| match e {
            // Closing the window keeps the app (and the hub) running in the tray; Quit in the tray menu ends it.
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if let Some(wv) = w.app_handle().get_webview_window(w.label()) {
                    window::save_bounds(&wv, &w.app_handle().state::<AppState>().dirs.data);
                }
                let _ = w.hide();
                api.prevent_close();
            }
            tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                if let Some(wv) = w.app_handle().get_webview_window(w.label()) {
                    window::save_bounds(&wv, &w.app_handle().state::<AppState>().dirs.data);
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(a) => a,
        Err(e) => {
            eprintln!("playbox-desktop: could not start: {e}");
            std::process::exit(1);
        }
    };
    app.run(|handle, event| match event {
        // Cmd-Q, the dock menu and the OS shutting down all take the same graceful path as the tray's Quit.
        RunEvent::ExitRequested {
            api, code: None, ..
        } => {
            api.prevent_exit();
            quit::graceful(handle);
        }
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
        RunEvent::Opened { urls } => {
            let st = handle.state::<AppState>();
            let ready = st.hub_ready.load(Ordering::Acquire);
            for u in urls {
                if let Some(path) = deeplink::parse_to_hub_path(u.as_str()) {
                    if ready {
                        window::navigate_path(handle, service::port(), &path);
                    } else if let Ok(mut q) = st.pending_links.lock() {
                        q.push(path);
                    }
                }
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows,
            ..
        } => {
            if !has_visible_windows {
                window::reveal(handle);
            }
        }
        _ => {}
    });
}
