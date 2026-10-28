//! Real microVM runs. Skipped (with a message) where libkrun, its signed helper, or Docker is missing.

use playbox_runner::{find_helper, KrunRunner, Runner, SandboxPolicy, TaskSpec};
use std::{path::PathBuf, process::Command, time::Instant};

fn ready() -> Option<KrunRunner> {
    if find_helper().is_err() {
        eprintln!("skipping: playbox-krun-helper not installed (scripts/install-krun.sh)");
        return None;
    }
    if !Command::new("docker").arg("info").output().map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("skipping: docker is not running (needed to export the root filesystem)");
        return None;
    }
    KrunRunner::locate(SandboxPolicy::default()).ok()
}

fn ws(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-krun-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sh(script: &str) -> TaskSpec {
    TaskSpec { timeout_secs: 60, mem_mib: 256, ..TaskSpec::new(&["sh", "-c", script]) }
}

#[tokio::test]
async fn runs_a_command_in_its_own_kernel_with_separate_streams_and_exit_code() {
    let Some(r) = ready() else { return };
    let w = ws("basic");
    let out = r.run(&sh("echo out; echo err >&2; uname -s; cat /proc/version | cut -c1-30; exit 7"), &w).await.unwrap();
    assert_eq!(out.exit_code, Some(7), "{out:?}");
    assert!(out.stdout.starts_with("out\nLinux\n"), "{out:?}");
    assert_eq!(out.stderr, "err\n");
    // The host is macOS; "Linux version" proves a guest kernel ran, not a process on the host.
    if cfg!(target_os = "macos") {
        assert!(out.stdout.contains("Linux version"), "{out:?}");
    }
}

#[tokio::test]
async fn the_workspace_is_shared_and_the_rest_of_the_filesystem_is_thrown_away() {
    let Some(r) = ready() else { return };
    let w = ws("fs");
    std::fs::write(w.join("in.txt"), "from host\n").unwrap();
    let out = r
        .run(&sh("cat in.txt; echo from-guest > out.txt; echo scratch > /root/leftover; pwd"), &w)
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout, "from host\n/workspace\n");
    assert_eq!(std::fs::read_to_string(w.join("out.txt")).unwrap(), "from-guest\n");

    // A second task starts from a clean root: the first one's file outside /workspace is gone.
    let out = r.run(&sh("ls /root/leftover 2>&1; echo done"), &w).await.unwrap();
    assert!(out.stdout.contains("No such file") && out.stdout.ends_with("done\n"), "{out:?}");
}

#[tokio::test]
async fn there_is_no_network_unless_asked_for() {
    let Some(r) = ready() else { return };
    let w = ws("net");
    let started = Instant::now();
    let out = r.run(&sh("wget -q -T 4 -O /dev/null http://1.1.1.1 2>&1; echo rc=$?"), &w).await.unwrap();
    assert!(out.stdout.contains("rc=1") || out.stdout.contains("unreachable"), "{out:?}");
    assert!(!out.stdout.contains("rc=0"), "{out:?}");
    assert!(started.elapsed().as_secs() < 30);
}

#[tokio::test]
async fn timeouts_kill_the_vm_and_report_it() {
    let Some(r) = ready() else { return };
    let w = ws("timeout");
    let spec = TaskSpec { timeout_secs: 3, ..sh("sleep 60") };
    let started = Instant::now();
    let out = r.run(&spec, &w).await.unwrap();
    assert!(out.timed_out, "{out:?}");
    assert!(started.elapsed().as_secs() < 20);
}

#[tokio::test]
async fn images_outside_the_policy_are_refused_before_anything_boots() {
    let Some(r) = ready() else { return };
    let w = ws("policy");
    let spec = TaskSpec { image: "ubuntu:latest".into(), ..sh("true") };
    let e = r.run(&spec, &w).await.unwrap_err();
    assert!(format!("{e:#}").contains("not allowed"), "{e:#}");
    let e = r.run(&TaskSpec { image: "--privileged".into(), ..sh("true") }, &w).await.unwrap_err();
    assert!(format!("{e:#}").contains("invalid image"), "{e:#}");
}

#[tokio::test]
async fn preflight_boots_a_real_vm() {
    let Some(r) = ready() else { return };
    r.preflight().await.unwrap();
}

#[tokio::test]
async fn a_missing_command_is_a_clean_failure_not_a_hang() {
    let Some(r) = ready() else { return };
    let w = ws("nocmd");
    let out = r.run(&TaskSpec { timeout_secs: 30, mem_mib: 256, ..TaskSpec::new(&["/no/such/program"]) }, &w).await.unwrap();
    assert!(!out.timed_out && out.exit_code != Some(0), "{out:?}");
}
