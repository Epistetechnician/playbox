//! A task stopped by the quota must also stop its *container*. This uses a stand-in `docker` program that
//! behaves like a runaway container (it keeps writing into the mounted workspace) and records every
//! `docker kill`, so it checks the runner's behaviour without needing a working Docker daemon.

use playbox_runner::{run_with_quota, DockerRunner, Quota, TaskSpec};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

const SHIM: &str = r#"#!/bin/sh
case "$1" in
  run)
    ws=""; name=""; prev=""
    for a in "$@"; do
      [ "$prev" = "--name" ] && name="$a"
      [ "$prev" = "-v" ] && ws="${a%%:/workspace}"
      prev="$a"
    done
    echo "$name" > "$SHIM_DIR/name"
    i=0
    while [ $i -lt 100 ]; do
      dd if=/dev/zero bs=1048576 count=10 >> "$ws/shim_probe.bin" 2>/dev/null
      sleep 0.2
      i=$((i+1))
    done ;;
  kill) echo "$2" >> "$SHIM_DIR/killed" ;;
  *) exit 0 ;;
esac
"#;

#[tokio::test]
async fn a_task_stopped_by_the_quota_gets_its_container_killed() {
    let base = std::env::temp_dir().join(format!("playbox-dockershim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (bin, ws): (PathBuf, PathBuf) = (base.join("bin"), base.join("ws"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    let shim = bin.join("docker");
    std::fs::write(&shim, SHIM).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    // This test binary holds only this test, so changing the process environment is safe.
    std::env::set_var("SHIM_DIR", &base);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );

    let spec = TaskSpec {
        timeout_secs: 60,
        ..TaskSpec::new(&["sh", "-c", "true"])
    };
    let started = Instant::now();
    let r = run_with_quota(
        &DockerRunner::default(),
        &spec,
        &ws,
        Quota {
            max_bytes: 30 << 20,
            max_entries: 1000,
        },
    )
    .await;
    let e = format!(
        "{:#}",
        r.expect_err("the runaway container must be stopped")
    );
    assert!(e.contains("scratch quota"), "{e}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );

    // The kill is sent asynchronously when the run is dropped; give it a moment.
    let name = std::fs::read_to_string(base.join("name"))
        .unwrap()
        .trim()
        .to_string();
    assert!(name.starts_with("playbox-"), "{name:?}");
    let mut killed = String::new();
    for _ in 0..30 {
        killed = std::fs::read_to_string(base.join("killed")).unwrap_or_default();
        if killed.contains(&name) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        killed.lines().any(|l| l.trim() == name),
        "docker kill {name} was never issued; saw {killed:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}
