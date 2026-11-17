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

/// A warm session reuses one sandbox: the second task skips the sandbox start, sees only its own workspace
/// (not the first task's files in `/sandbox` or `/tmp`), and a different session gets a different sandbox.
#[tokio::test]
async fn warm_session_is_faster_and_leaves_nothing_behind() {
    if !enabled() {
        return;
    }
    let mk = |tag: &str| {
        let ws = std::env::temp_dir().join(format!("pb-osh-warm-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("a.txt"), tag).unwrap();
        ws
    };
    let r = OpenShellRunner::new(SandboxPolicy::default());
    let spec = |s: &str| TaskSpec::new(&["sh", "-c", s]);
    let (w1, w2, w3) = (mk("one"), mk("two"), mk("three"));

    let t = std::time::Instant::now();
    let a = r
        .run_warm(
            &spec("hostname; echo secret > /tmp/left; echo s > /sandbox/left; ls /sandbox"),
            &w1,
            "member-a",
        )
        .await
        .unwrap();
    let cold = t.elapsed();
    let t = std::time::Instant::now();
    let b = r
        .run_warm(
            &spec("hostname; ls /sandbox; ls /tmp; cat a.txt"),
            &w2,
            "member-a",
        )
        .await
        .unwrap();
    let warm = t.elapsed();
    let host =
        |o: &playbox_runner::TaskOutput| o.stdout.lines().next().unwrap_or_default().to_string();
    assert_eq!(a.exit_code, Some(0), "{a:?}");
    assert_eq!(
        host(&a),
        host(&b),
        "same session must reuse the sandbox: {a:?} {b:?}"
    );
    assert!(
        !b.stdout.contains("left"),
        "the first task's files leaked into the second: {b:?}"
    );
    assert!(
        b.stdout.contains("two"),
        "the second task must see its own workspace: {b:?}"
    );
    assert!(
        warm < cold,
        "the warm run ({warm:?}) should beat the cold one ({cold:?})"
    );

    let c = r
        .run_warm(&spec("hostname"), &w3, "member-b")
        .await
        .unwrap();
    assert_ne!(
        host(&a),
        host(&c),
        "another session must not share a sandbox"
    );

    r.end_warm("member-a").await;
    r.end_warm("member-b").await;
    let d = r
        .run_warm(&spec("hostname"), &w1, "member-a")
        .await
        .unwrap();
    assert_ne!(
        host(&a),
        host(&d),
        "a sandbox ended with end_warm must not come back"
    );
    r.end_warm("member-a").await;
}

/// A workspace of a real repository is megabytes: it goes in and comes back as a tar stream on every task.
/// Regression: a ~9 MB workspace used to stall the upload until the caller gave up.
#[tokio::test]
async fn large_workspace_round_trips() {
    if !enabled() {
        return;
    }
    let ws = std::env::temp_dir().join(format!("pb-osh-big-{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    // 9 MiB of incompressible bytes in a few hundred files, so size and file count both matter.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut block = vec![0u8; 30 * 1024];
    for i in 0..300 {
        for b in block.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        std::fs::write(ws.join(format!("f{i}.bin")), &block).unwrap();
    }
    let want = std::fs::read(ws.join("f299.bin")).unwrap();
    let r = OpenShellRunner::new(SandboxPolicy::default());
    let spec = TaskSpec::new(&["sh", "-c", "ls | wc -l; echo changed > f0.bin"]);
    let t = std::time::Instant::now();
    let out = r.run(&spec, &ws).await.unwrap();
    eprintln!("large workspace round trip: {:?}", t.elapsed());
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(out.stdout.trim().starts_with("300"), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(ws.join("f0.bin")).unwrap().trim(),
        "changed"
    );
    assert_eq!(std::fs::read(ws.join("f299.bin")).unwrap(), want);
}
