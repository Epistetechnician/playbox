#![allow(clippy::unwrap_used)]
//! The real hub under the real supervisor: it starts, answers its health probe, is stopped, and leaves nothing listening.
//! Skipped (with a message) where python3 or the hub script is not available.

use pbd_core::hub::{self, HubConfig};
use pbd_core::layout::Dirs;
use pbd_core::supervisor::{State, Supervisor};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
#[cfg(unix)]
fn the_real_hub_starts_is_healthy_and_stops_cleanly() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../ui");
    let script = root.join("playbox_hub.py");
    if !script.is_file()
        || std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
    {
        eprintln!("skipped: no python3 or no hub script");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("pbd-realhub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let dirs = Dirs {
        home: tmp.clone(),
        data: tmp.join("data"),
        logs: tmp.join("logs"),
        hub_home: tmp.join("hub"),
    };
    let port = free_port();
    let cfg = HubConfig {
        python: "python3".into(),
        script,
        port,
        extra_args: vec![],
    };
    let mut spec = hub::spec(&cfg, &dirs);
    spec.args.push("--mcp".into());
    spec.args.push("/usr/bin/true".into()); // the hub needs a path to a sidecar; this test never opens a room
    let (tx, rx) = mpsc::channel();
    let sup = Supervisor::start(spec, move |e| {
        let _ = tx.send(e.state);
    });
    let end = Instant::now() + Duration::from_secs(40);
    let mut healthy = false;
    while Instant::now() < end {
        if let Ok(s) = rx.recv_timeout(Duration::from_millis(200)) {
            if s == State::Healthy {
                healthy = true;
                break;
            }
        }
    }
    let log = std::fs::read_to_string(dirs.logs.join("hub.log")).unwrap_or_default();
    assert!(healthy, "hub never became healthy. log:\n{log}");
    assert!(hub::probe(port).check(Duration::from_secs(2)));
    assert!(
        dirs.hub_home.is_dir(),
        "the hub should use the home it was given"
    );
    sup.stop();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err(),
        "nothing should still listen after stop"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
