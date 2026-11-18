//! Leaving: stop the step-up bridge, background watcher and hub politely (it takes its tunnel down with it) before the process ends,
//! without freezing the UI while it does.

use crate::AppState;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager};

pub fn graceful(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        state.stop_bg.store(true, Ordering::Release);
        let bridge = state.stepup.lock().ok().and_then(|mut g| g.take());
        if let Some(mut b) = bridge {
            b.stop();
        }
        let sup = state.supervisor.lock().ok().and_then(|mut g| g.take());
        if let Some(s) = sup {
            s.stop();
        }
        app.exit(0);
    });
}

/// `kill`, a terminal's Ctrl-C and a closing session all end the app the same graceful way, so the hub is never left running unowned.
#[cfg(unix)]
pub fn on_signals(app: &AppHandle) {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let Ok(mut sigs) = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP]) else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        if sigs.forever().next().is_some() {
            graceful(&app);
        }
    });
}

#[cfg(not(unix))]
pub fn on_signals(_app: &AppHandle) {}
