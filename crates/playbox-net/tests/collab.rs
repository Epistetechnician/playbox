//! End to end: pull a repo, run a sandboxed task remotely, apply the patch, push back.
//! The task test needs a running Docker daemon and is skipped without one.

use playbox_net::{join_link, new_secret_key, HostConfig, Host};
use playbox_runner::{DockerRunner, TaskSpec};
use std::{path::Path, process::Command, sync::Arc, time::Duration};

/// Whether Docker answers within a few seconds. A hung daemon would otherwise stall every test that wants it for a minute.
fn docker_answers() -> bool {
    use std::process::Stdio;
    let Ok(mut child) = Command::new("docker").arg("info").stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return false };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < std::time::Duration::from_secs(8) => std::thread::sleep(std::time::Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                return false;
            }
        }
    }
}


fn sh(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    sh(dir, &["init", "-q", "-b", "main"]);
    sh(dir, &["config", "user.email", "t@example.com"]);
    sh(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("hello.txt"), "hello\n").unwrap();
    sh(dir, &["add", "-A"]);
    sh(dir, &["commit", "-q", "-m", "init"]);
}

fn docker_up() -> bool {
    docker_answers()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[tokio::test]
async fn pull_task_patch_push() {
    if !docker_up() {
        eprintln!("skipping: docker is not running");
        return;
    }
    let host_repo = scratch("host");
    init_repo(&host_repo);
    let cfg = HostConfig { repo: Some(host_repo.clone()), runner: Some(Arc::new(DockerRunner::default())), ..Default::default() };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let client = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();

    // Pull main into a fresh local repo.
    let (head, bundle) = client.pull("main").await.unwrap();
    assert_eq!(head, sh(&host_repo, &["rev-parse", "main"]));
    let local = scratch("local");
    std::fs::create_dir_all(&local).unwrap();
    sh(&local, &["init", "-q", "-b", "main"]);
    sh(&local, &["config", "user.email", "g@example.com"]);
    sh(&local, &["config", "user.name", "g"]);
    playbox_git::bundle_import(&local, &bundle, "refs/heads/main", "refs/remotes/playbox/main").await.unwrap();
    sh(&local, &["checkout", "-q", "-B", "main", "refs/remotes/playbox/main"]);
    assert_eq!(std::fs::read_to_string(local.join("hello.txt")).unwrap(), "hello\n");

    // Run a task remotely; it edits one file and adds another.
    let spec = TaskSpec::new(&["sh", "-c", "echo remote >> hello.txt && echo built > out.txt && cat hello.txt"]);
    let (runner, out, patch) = client.run_task(&head, spec).await.unwrap();
    assert_eq!(runner, "docker");
    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, "hello\nremote\n");
    assert!(!patch.is_empty());
    // The host's own checkout is untouched.
    assert_eq!(std::fs::read_to_string(host_repo.join("hello.txt")).unwrap(), "hello\n");

    // Apply the patch locally, commit, push back.
    playbox_git::apply_patch(&local, &patch).await.unwrap();
    assert_eq!(std::fs::read_to_string(local.join("out.txt")).unwrap(), "built\n");
    sh(&local, &["commit", "-q", "-m", "apply remote result"]);
    let bundle = playbox_git::bundle_create(&local, "main").await.unwrap();
    let reference = client.push("main", &bundle).await.unwrap();
    assert!(reference.starts_with("refs/playbox/members/"));
    assert_eq!(sh(&host_repo, &["rev-parse", &reference]), sh(&local, &["rev-parse", "main"]));
    // main on the host did not move.
    assert_eq!(sh(&host_repo, &["rev-parse", "main"]), head);

    client.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn task_has_no_network_and_times_out() {
    if !docker_up() {
        eprintln!("skipping: docker is not running");
        return;
    }
    let host_repo = scratch("host2");
    init_repo(&host_repo);
    let cfg = HostConfig { repo: Some(host_repo.clone()), runner: Some(Arc::new(DockerRunner::default())), ..Default::default() };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let client = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();

    // The member's grant forbids network, so asking for it is refused outright.
    let mut ask = TaskSpec::new(&["true"]);
    ask.network = true;
    assert!(client.run_task("main", ask).await.unwrap_err().to_string().contains("not authorized"));

    // Without it, the container really has no route out.
    let spec = TaskSpec::new(&["sh", "-c", "wget -q -T 3 -O - http://example.com >/dev/null 2>&1; echo $?"]);
    let (_, out, _) = client.run_task("main", spec).await.unwrap();
    assert_ne!(out.stdout.trim(), "0", "network should be unreachable");

    let mut spec = TaskSpec::new(&["sleep", "30"]);
    spec.timeout_secs = 2;
    let (_, out, _) = client.run_task("main", spec).await.unwrap();
    assert!(out.timed_out);

    client.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn bad_refs_and_missing_features_are_refused() {
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap(); // no repo, no runner
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let client = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    assert!(client.pull("main").await.is_err());
    assert!(client.run_task("main", TaskSpec::new(&["true"])).await.is_err());
    client.close().await;
    host.shutdown().await.unwrap();

    let repo = scratch("host3");
    init_repo(&repo);
    let cfg = HostConfig { repo: Some(repo), ..Default::default() };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let client = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    assert!(client.pull("--upload-pack=evil").await.is_err());
    assert!(client.pull("main..other").await.is_err());
    client.close().await;
    host.shutdown().await.unwrap();
}
