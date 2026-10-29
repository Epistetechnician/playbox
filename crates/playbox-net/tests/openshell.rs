//! A member's task through a real host running the OpenShell runner. Needs the `openshell` CLI, a
//! reachable gateway, and `PLAYBOX_ALLOW_MISSING_LIMITS=1`; runs only when `PLAYBOX_TEST_OPENSHELL` is set.

use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use playbox_runner::{Limits, OpenShellRunner, SandboxPolicy, TaskSpec};
use std::{process::Command, sync::Arc, time::Duration};

fn git(dir: &std::path::Path, args: &[&str]) {
    assert!(Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "git {args:?}");
}

#[tokio::test]
async fn a_member_reaches_only_the_hosts_the_grant_lists() {
    if std::env::var_os("PLAYBOX_TEST_OPENSHELL").is_none() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("playbox-osh-net-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let cfg = HostConfig {
        repo: Some(dir),
        runner: Some(Arc::new(OpenShellRunner::new(SandboxPolicy::default()))),
        limits: Limits { allowed_hosts: vec!["example.com".into()], timeout_secs: 120, ..Default::default() },
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();

    let script = "wget -q -T 15 -O /dev/null https://example.com && echo LISTED_OK; wget -q -T 15 -O /dev/null https://www.wikipedia.org && echo OTHER_OK; echo done";
    let spec = TaskSpec { allowed_hosts: vec!["example.com".into()], timeout_secs: 90, ..TaskSpec::new(&["sh", "-c", script]) };
    let (runner, out, _) = c.run_task("main", spec).await.unwrap();
    assert_eq!(runner, "openshell");
    assert!(out.stdout.contains("LISTED_OK") && !out.stdout.contains("OTHER_OK"), "{out:?}");
    assert!(out.policy_digest.is_some(), "the digest of the policy the task ran under travels with the result");

    // A host the grant does not list never reaches the runner.
    let spec = TaskSpec { allowed_hosts: vec!["www.wikipedia.org".into()], ..TaskSpec::new(&["true"]) };
    assert!(format!("{:#}", c.run_task("main", spec).await.unwrap_err()).contains("not authorized"));
}
