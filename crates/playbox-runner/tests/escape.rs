//! Sandbox escape drill: a battery of breakout attempts run from inside a real libkrun guest.
//! Every probe must fail. Skipped (with a message) where libkrun, its signed helper, or Docker is missing.
//!
//! Run it whenever the sandbox, the helper, the image or the model changes (docs/security-roadmap.md, "Escape drill").

use playbox_runner::{find_helper, KrunRunner, Runner, SandboxPolicy, TaskSpec};
use std::{path::PathBuf, process::Command};

/// A case that cannot run here: a pass with a loud note by default, a failure when `PLAYBOX_CONFINE_REQUIRE_ALL` is set (CI's
/// `krun` job sets it), so a skipped check can never look like a passed one where the environment is meant to support it.
fn skip(why: &str) {
    eprintln!("skipping: {why}");
    assert!(
        std::env::var_os("PLAYBOX_CONFINE_REQUIRE_ALL").is_none(),
        "PLAYBOX_CONFINE_REQUIRE_ALL is set and this could not run: {why}"
    );
}

/// A runner, where the helper is installed. Enough for a request that is refused before any image is fetched.
fn runner() -> Option<KrunRunner> {
    if find_helper().is_err() {
        skip("playbox-krun-helper not installed (scripts/install-krun.sh)");
        return None;
    }
    KrunRunner::locate(SandboxPolicy::default()).ok()
}

/// A runner that can boot a guest: the image comes through Docker.
fn ready() -> Option<KrunRunner> {
    let r = runner()?;
    if !Command::new("docker")
        .arg("info")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        skip("docker is not running");
        return None;
    }
    Some(r)
}

fn ws(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-escape-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The task is root in its own guest kernel, so mounting filesystems or writing the guest's `/proc/sys` succeeds and is not
/// an escape; the probes below only count reaching the host or its network.
/// No quotes anywhere: they do not survive the trip into the guest's command line.
/// Each probe prints `ESCAPED:<name>` if it got somewhere it must not. Nothing printed means the sandbox held.
const PROBES: &str = r#"
t() { n=$1; shift; if $@ >/dev/null 2>&1; then echo ESCAPED:$n; fi; }
w_sys() { echo 1 > /sys/kernel/uevent_helper; }
r_mem() { head -c1 /dev/mem; }
t canary              true
t has_wget            which wget
t host_users_dir      test -e /Users
t host_volumes        test -e /Volumes
t host_home_ssh       test -e HOST_HOME/.ssh
t host_ssh_keys       test -e /root/.ssh/id_ed25519
t docker_socket       test -S /var/run/docker.sock
t write_sys           w_sys
t kmem                test -r /dev/kmem
t dev_mem             r_mem
t loopback_host       wget -q -T 2 -O /dev/null http://127.0.0.1:1/
t metadata_service    wget -q -T 2 -O /dev/null http://169.254.169.254/
t internet            wget -q -T 2 -O /dev/null http://1.1.1.1/
t secrets_dir         test -e /run/secrets
t workspace_parent    test -e /workspace/../escape-marker
"#;

// REQ: BOX-6
#[tokio::test]
async fn nothing_in_the_escape_drill_gets_out_of_a_libkrun_guest() {
    let Some(r) = ready() else { return };
    let w = ws("drill");
    // The host user's own home, derived here so no one's name is written in the probe list.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/nonexistent-host-home".into());
    // libkrun rejects control characters (newlines) in the guest's command line, so the script goes in as one line.
    let script = PROBES
        .replace("HOST_HOME", &home)
        .trim()
        .lines()
        .collect::<Vec<_>>()
        .join("; ");
    let spec = TaskSpec {
        timeout_secs: 90,
        mem_mib: 256,
        ..TaskSpec::new(&["sh", "-c", &script])
    };
    let out = r.run(&spec, &w).await.unwrap();
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    // The canaries always fire: the first shows the probes ran, the second that the guest has the `wget` the network probes
    // use. Without them a quiet result would mean nothing.
    for canary in ["ESCAPED:canary", "ESCAPED:has_wget"] {
        assert!(
            out.stdout.contains(canary),
            "the probes did not run ({canary} missing): {out:?}"
        );
    }
    // On x86-64 the guest kernel exposes `/sys/kernel/uevent_helper` and `/dev/mem` to root. Both are the guest's own
    // kernel, not the host's, so they are reported but do not fail the drill.
    let own_kernel = ["ESCAPED:write_sys", "ESCAPED:dev_mem"];
    let real: Vec<_> = out
        .stdout
        .lines()
        .filter(|l| {
            l.starts_with("ESCAPED:")
                && !matches!(*l, "ESCAPED:canary" | "ESCAPED:has_wget")
                && !own_kernel.contains(l)
        })
        .collect();
    for l in out.stdout.lines().filter(|l| own_kernel.contains(l)) {
        eprintln!("note: {l} (inside the guest's own kernel; not a host escape)");
    }
    assert!(
        real.is_empty(),
        "the guest reached something it must not: {real:?}"
    );
}

// REQ: BOX-5
#[tokio::test]
async fn a_task_asking_for_a_network_on_an_unfiltered_host_is_refused() {
    // Refused before any image is fetched, so this needs the helper but not Docker.
    let Some(r) = runner() else { return };
    // `locate` builds an unconfined runner (no `--confine`), so the refusal applies on Linux too, unless the owner opted in.
    if playbox_runner::env_opt_in("PLAYBOX_ALLOW_UNFILTERED_NETWORK") {
        eprintln!("skipping: PLAYBOX_ALLOW_UNFILTERED_NETWORK is set, so the request is accepted");
        return;
    }
    let w = ws("refuse");
    let spec = TaskSpec {
        network: true,
        mem_mib: 256,
        ..TaskSpec::new(&["true"])
    };
    let err = r.run(&spec, &w).await.unwrap_err();
    assert!(err.to_string().contains("cannot filter"), "{err:#}");
}
