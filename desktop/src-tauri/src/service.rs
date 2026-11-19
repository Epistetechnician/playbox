//! Starts the hub under supervision, turns its state changes into tray text and window navigation, and polls rooms/attention
//! and queued OS deep links while the hub is healthy.

use crate::{notify, tray, window, AppState};
use pbd_core::{
    deeplink, hub, status,
    supervisor::{Event, State, Supervisor},
    watch::{DiffTracker, HubSnapshot, LoopbackClient},
};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
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
    if std::env::var_os("APPDIR").is_some() || std::env::var_os("APPIMAGE").is_some() {
        // AppRun prepends $APPDIR/usr/lib to LD_LIBRARY_PATH, overriding python-build-standalone's DT_RUNPATH and host git/ssh libs.
        spec.env_remove.push("LD_LIBRARY_PATH".into());
    }
    spec.env.extend(env);
    let (handle, port) = (app.clone(), cfg.port);
    Supervisor::start(spec, move |e: Event| on_event(&handle, port, &e))
}

/// Background worker: drains OS deep links (`playbox://`, `web+playbox:`) and polls `/api/rooms` for tray menus, badges and notifications.
pub fn start_watcher(app: &AppHandle) {
    let app = app.clone();
    let hub_port = port();
    std::thread::spawn(move || {
        let mut client = LoopbackClient::default();
        let mut diff = DiffTracker::default();
        let mut last_net = hub::read_network_mode(&app.state::<AppState>().dirs.hub_home);
        let mut last_poll = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        loop {
            let st = app.state::<AppState>();
            if st.stop_bg.load(Ordering::Acquire) {
                break;
            }
            let cur_net = hub::read_network_mode(&st.dirs.hub_home);
            if cur_net != last_net {
                last_net = cur_net;
                st.hub_ready.store(false, Ordering::Release);
                let old = st.supervisor.lock().ok().and_then(|mut g| g.take());
                if let Some(s) = old {
                    s.stop();
                }
                let fresh = start(&app);
                if let Ok(mut g) = st.supervisor.lock() {
                    *g = Some(fresh);
                }
            }
            let ready = st.hub_ready.load(Ordering::Acquire);
            if ready {
                // 1. Drain deep links sent from a second launch or queued before the hub finished starting.
                let mut links = deeplink::drain_hub_paths(&st.dirs.data);
                if let Ok(mut pending) = st.pending_links.lock() {
                    if !pending.is_empty() {
                        let mut first = std::mem::take(&mut *pending);
                        first.extend(links);
                        links = first;
                    }
                }
                for path in links {
                    window::navigate_path(&app, hub_port, &path);
                }

                // 2. Poll rooms & attention gently every 5 seconds.
                if last_poll.elapsed() >= Duration::from_secs(5) {
                    last_poll = Instant::now();
                    if let Some(snap) = client.fetch_snapshot(hub_port, &st.dirs.hub_home) {
                        let focused = window::is_focused_and_visible(&app);
                        for alert in diff.update(&snap, focused) {
                            notify::send(&alert);
                        }
                        tray::set_snapshot(&app, snap);
                    }
                }
            } else {
                diff.reset();
            }
            std::thread::sleep(Duration::from_millis(450));
        }
    });
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
    let st = app.state::<AppState>();
    match e.state {
        State::Healthy | State::Attached => {
            st.hub_ready.store(true, Ordering::Release);
            let first_link = st.pending_links.lock().ok().and_then(|mut q| {
                if q.is_empty() {
                    None
                } else {
                    Some(q.remove(0))
                }
            });
            if let Some(path) = first_link {
                window::navigate_path(app, port, &path);
            } else {
                window::show_hub(app, port);
            }
        }
        State::Failed { .. } => {
            st.hub_ready.store(false, Ordering::Release);
            tray::set_snapshot(app, HubSnapshot::default());
            window::say(
                app,
                &format!(
                    "{} Use Restart hub or Show logs in the Playbox menu bar icon.",
                    line.text
                ),
            );
        }
        _ => {
            st.hub_ready.store(false, Ordering::Release);
            window::say(app, &line.text);
        }
    }
}
