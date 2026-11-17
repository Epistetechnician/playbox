//! Starts the hub under supervision and turns its state changes into tray text and window navigation.

use crate::{tray, window, AppState};
use pbd_core::{
    hub, status,
    supervisor::{Event, State, Supervisor},
};
use tauri::{AppHandle, Manager};

fn port() -> u16 {
    std::env::var("PLAYBOX_DESKTOP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(pbd_core::identity::Product::HUB_PORT)
}

pub fn start(app: &AppHandle) -> Supervisor {
    let state = app.state::<AppState>();
    let resources = app.path().resource_dir().unwrap_or_default();
    let mut cfg = hub::locate(&resources, port());
    // Development: run the hub from a checkout instead of the bundled copy.
    if let Some(s) = std::env::var_os("PLAYBOX_DESKTOP_HUB_SCRIPT") {
        cfg.script = s.into();
    }
    // Extra hub flags (for example the Cloudflare tunnel options), whitespace-separated.
    if let Ok(extra) = std::env::var("PLAYBOX_DESKTOP_HUB_ARGS") {
        cfg.extra_args = extra.split_whitespace().map(Into::into).collect();
    }
    let spec = hub::spec(&cfg, &state.dirs);
    let (handle, port) = (app.clone(), cfg.port);
    Supervisor::start(spec, move |e: Event| on_event(&handle, port, &e))
}

fn on_event(app: &AppHandle, port: u16, e: &Event) {
    let line = status::describe(&e.name, &e.state);
    tray::set_status(app, &line);
    match e.state {
        State::Healthy | State::Attached => window::show_hub(app, port),
        State::Failed { .. } => window::say(
            app,
            &format!("{} See the log from the tray menu.", line.text),
        ),
        _ => window::say(app, &line.text),
    }
}
