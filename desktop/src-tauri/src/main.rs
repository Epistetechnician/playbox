//! Playbox Desktop: a thin native shell. All decisions live in `pbd-core`; this crate only connects them to a window, a tray item and the
//! operating system's lifecycle events.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod quit;
mod service;
mod tray;
mod window;

use pbd_core::{claim::Claim, layout};
use std::sync::Mutex;
use tauri::{Manager, RunEvent};

/// What the running app owns. Dropping it releases the claim.
pub struct AppState {
    pub dirs: layout::Dirs,
    pub supervisor: Mutex<Option<pbd_core::supervisor::Supervisor>>,
    _claim: Claim,
}

fn main() {
    let dirs = match layout::from_process_env() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("playbox-desktop: {e}");
            std::process::exit(1);
        }
    };
    let claim = match Claim::acquire(&dirs.data) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("playbox-desktop: {e}");
            std::process::exit(0); // not an error: the other one is the app
        }
    };
    let background = std::env::args().any(|a| a == "--background");
    let state = AppState {
        dirs,
        supervisor: Mutex::new(None),
        _claim: claim,
    };

    let app = tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            quit::on_signals(app.handle());
            tray::build(app.handle())?;
            let win = window::ensure(app.handle())?;
            if !background {
                let _ = win.show();
            }
            let sup = service::start(app.handle());
            if let Ok(mut g) = app.state::<AppState>().supervisor.lock() {
                *g = Some(sup);
            }
            Ok(())
        })
        .on_window_event(|w, e| {
            // Closing the window keeps the app (and the hub) running in the tray; Quit in the tray menu ends it.
            if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                let _ = w.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(a) => a,
        Err(e) => {
            eprintln!("playbox-desktop: could not start: {e}");
            std::process::exit(1);
        }
    };
    app.run(|handle, event| {
        // Cmd-Q, the dock menu and the OS shutting down all take the same graceful path as the tray's Quit.
        if let RunEvent::ExitRequested {
            api, code: None, ..
        } = event
        {
            api.prevent_exit();
            quit::graceful(handle);
        }
    });
}
