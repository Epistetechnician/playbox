//! Starts the hub under supervision and turns its state changes into tray text and window navigation.

use crate::{tray, window, AppState};
use pbd_core::{
    hub, status,
    supervisor::{Event, State, Supervisor},
};
use tauri::{AppHandle, Manager};

pub fn port() -> u16 {
    std::env::var("PLAYBOX_DESKTOP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(pbd_core::identity::Product::HUB_PORT)
}

pub fn start(app: &AppHandle) -> Supervisor {
    let state = app.state::<AppState>();
    let resources = std::env::current_exe()
        .ok()
        .and_then(|e| pbd_core::layout::resources_for_exe(&e))
        .or_else(|| app.path().resource_dir().ok())
        .unwrap_or_default();
    let mut cfg = hub::locate(&resources, port());
    // Development: run the hub from a checkout instead of the bundled copy.
    if let Some(s) = std::env::var_os("PLAYBOX_DESKTOP_HUB_SCRIPT") {
        cfg.script = s.into();
    }
    // A bundled `playbox` binary is the hub's CLI and MCP server (agent setup, machine checks); fall back to whatever is on PATH.
    let bin = resources.join(if cfg!(windows) {
        "bin/playbox.exe"
    } else {
        "bin/playbox"
    });
    let mut env = vec![];
    if bin.is_file() {
        env.push(("PLAYBOX_CLI".into(), bin.clone().into_os_string()));
        cfg.extra_args = vec!["--mcp".into(), bin.into_os_string()];
    }
    // Extra hub flags (for example the Cloudflare tunnel options), whitespace-separated.
    if let Ok(extra) = std::env::var("PLAYBOX_DESKTOP_HUB_ARGS") {
        cfg.extra_args
            .extend(extra.split_whitespace().map(Into::into));
    }
    let mut spec = hub::spec(&cfg, &state.dirs);
    spec.env.extend(env);
    let (handle, port) = (app.clone(), cfg.port);
    Supervisor::start(spec, move |e: Event| on_event(&handle, port, &e))
}

/// Every state change and its detail goes to `desktop.log` next to the hub's log, so "the hub did not start" always has a reason on disk.
fn record(app: &AppHandle, e: &Event) {
    use std::io::Write;
    let dir = &app.state::<AppState>().dirs.logs;
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("desktop.log"))
    {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = writeln!(f, "{t} {} {:?}: {}", e.name, e.state, e.detail);
    }
}

fn on_event(app: &AppHandle, port: u16, e: &Event) {
    record(app, e);
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
