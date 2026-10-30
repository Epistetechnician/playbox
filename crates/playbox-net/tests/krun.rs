//! The whole stack with microVM isolation: a guest runs a command over A2A on a host whose tasks
//! each boot a libkrun VM. Skipped where the signed helper or Docker is unavailable.

use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use playbox_runner::{find_helper, runner_by_name, Isolation, TaskSpec};
use std::{path::Path, process::Command, time::Duration};

/// Whether Docker answers within a few seconds. A hung daemon would otherwise stall every test that wants it for a minute.
fn docker_answers() -> bool {
    use std::process::Stdio;
    let Ok(mut child) = Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < std::time::Duration::from_secs(8) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                return false;
            }
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn a_guest_command_runs_in_a_microvm_and_comes_back_as_a_patch() {
    let docker = docker_answers();
    if find_helper().is_err() || !docker {
        eprintln!("skipping: libkrun helper or docker unavailable");
        return;
    }
    let repo = std::env::temp_dir().join(format!("playbox-netkrun-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.com"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "i"]);

    let runner = runner_by_name("libkrun", Default::default()).unwrap();
    assert_eq!(runner.isolation(), Isolation::MicroVm);
    let cfg = HostConfig {
        repo: Some(repo.clone()),
        runner: Some(runner),
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg)
        .await
        .expect("host starts (runs the VM smoke test)");
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let client = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();

    // The host attests to the runner it uses.
    assert_eq!(
        client
            .verify_host(playbox_core::attest::Tier::T0, &[])
            .await
            .unwrap()
            .statement
            .measurements
            .runner,
        "libkrun"
    );

    let spec = TaskSpec {
        timeout_secs: 60,
        mem_mib: 256,
        ..TaskSpec::new(&["sh", "-c", "echo guest >> a.txt; uname -s"])
    };
    let (runner, out, patch) = client.run_task("main", spec).await.unwrap();
    assert_eq!(runner, "libkrun");
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout, "Linux\n");
    assert!(
        String::from_utf8_lossy(&patch).contains("+guest"),
        "{}",
        String::from_utf8_lossy(&patch)
    );
    // The host's own checkout is untouched.
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "a\n");

    // Asking for network beyond the grant is still refused before any VM boots.
    let e = client
        .run_task(
            "main",
            TaskSpec {
                network: true,
                ..TaskSpec::new(&["true"])
            },
        )
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("not authorized"), "{e:#}");
}
