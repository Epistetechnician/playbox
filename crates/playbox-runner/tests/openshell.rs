//! Runs real tasks through `OpenShell`. Needs the `openshell` CLI and a reachable gateway
//! (`OPENSHELL_GATEWAY_ENDPOINT`), so it only runs when `PLAYBOX_TEST_OPENSHELL` is set.

use playbox_runner::{OpenShellRunner, Runner, SandboxPolicy, TaskSpec};

fn enabled() -> bool {
    std::env::var_os("PLAYBOX_TEST_OPENSHELL").is_some()
}

#[tokio::test]
async fn workspace_round_trips_and_exit_code_propagates() {
    if !enabled() {
        return;
    }
    let ws = std::env::temp_dir().join(format!("pb-osh-{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "hi").unwrap();
    std::fs::write(ws.join("gone.txt"), "x").unwrap();

    let r = OpenShellRunner::new(SandboxPolicy::default());
    let spec = TaskSpec::new(&[
        "sh",
        "-c",
        "cat a.txt; echo new > b.txt; rm gone.txt; echo oops >&2; id -u; exit 3",
    ]);
    let out = r.run(&spec, &ws).await.unwrap();

    assert_eq!(out.exit_code, Some(3));
    assert!(out.stdout.starts_with("hi"), "{out:?}");
    assert!(
        out.stdout.contains("1000"),
        "should run as the non-root sandbox user: {out:?}"
    );
    assert!(out.stderr.contains("oops"), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(ws.join("b.txt")).unwrap().trim(),
        "new"
    );
    assert!(
        !ws.join("gone.txt").exists(),
        "deletions must come back too"
    );
}

#[tokio::test]
async fn no_network_and_read_only_system() {
    if !enabled() {
        return;
    }
    let ws = std::env::temp_dir().join(format!("pb-osh-net-{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    let r = OpenShellRunner::new(SandboxPolicy::default());
    let spec = TaskSpec::new(&["sh", "-c", "touch /usr/x 2>/dev/null && echo WROTE; wget -q -T 5 -O- http://1.1.1.1 2>/dev/null && echo NET; echo done"]);
    let out = r.run(&spec, &ws).await.unwrap();
    assert!(
        out.stdout.contains("done") && !out.stdout.contains("WROTE") && !out.stdout.contains("NET"),
        "{out:?}"
    );
}

#[tokio::test]
async fn timeout_kills_the_task() {
    if !enabled() {
        return;
    }
    let ws = std::env::temp_dir().join(format!("pb-osh-to-{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    let r = OpenShellRunner::new(SandboxPolicy::default());
    let mut spec = TaskSpec::new(&["sleep", "60"]);
    spec.timeout_secs = 3;
    let out = r.run(&spec, &ws).await.unwrap();
    assert!(out.timed_out || out.exit_code != Some(0), "{out:?}");
}

#[tokio::test]
async fn network_reaches_only_the_allowed_hosts() {
    if !enabled() {
        return;
    }
    let ws = std::env::temp_dir().join(format!("pb-osh-hosts-{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    let r = OpenShellRunner::new(SandboxPolicy::default());
    let script = "wget -q -T 15 -O /dev/null https://example.com && echo ALLOWED_OK; wget -q -T 15 -O /dev/null https://www.wikipedia.org && echo OTHER_OK; echo done";
    let spec = TaskSpec {
        allowed_hosts: vec!["example.com".into()],
        timeout_secs: 90,
        ..TaskSpec::new(&["sh", "-c", script])
    };
    let out = r.run(&spec, &ws).await.unwrap();
    assert!(
        out.stdout.contains("ALLOWED_OK"),
        "allowed host unreachable: {out:?}"
    );
    assert!(
        !out.stdout.contains("OTHER_OK"),
        "a host outside the list was reachable: {out:?}"
    );
    // The digest names the exact policy the task ran under.
    let want = blake3::hash(
        playbox_runner::openshell_policy_yaml(&spec.allowed_hosts)
            .unwrap()
            .as_bytes(),
    )
    .to_hex()
    .to_string();
    assert_eq!(out.policy_digest.as_deref(), Some(want.as_str()));
}
