//! End-to-end stateful session (`keep = true`) and revocation teardown tests over a real Playbox host.

use async_trait::async_trait;
use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use playbox_runner::{Isolation, Limits, Runner, SyncMode, TaskOutput, TaskSpec};
use std::{
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct RecordingRunner {
    calls: Mutex<Vec<(String, String, SyncMode, String)>>,
    ended: Mutex<Vec<String>>,
}

#[async_trait]
impl Runner for RecordingRunner {
    fn name(&self) -> &'static str {
        "recording-session"
    }

    fn isolation(&self) -> Isolation {
        Isolation::MicroVm
    }

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> anyhow::Result<TaskOutput> {
        let out = Command::new(&spec.cmd[0])
            .args(&spec.cmd[1..])
            .current_dir(workspace)
            .output()?;
        Ok(TaskOutput {
            exit_code: out.status.code(),
            timed_out: false,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            policy_digest: Some("test-policy-digest".into()),
            secrets_used: Vec::new(),
        })
    }

    async fn run_session(
        &self,
        spec: &TaskSpec,
        workspace: &Path,
        session: &str,
        base_commit: &str,
        sync: SyncMode,
    ) -> anyhow::Result<TaskOutput> {
        self.calls.lock().unwrap().push((
            session.to_string(),
            base_commit.to_string(),
            sync,
            workspace.display().to_string(),
        ));
        self.run(spec, workspace).await
    }

    async fn end_warm(&self, session: &str) {
        self.ended.lock().unwrap().push(session.to_string());
    }
}

fn init_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(st.status.success(), "git {args:?}: {:?}", st);
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(dir.join("base.txt"), "hello\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
}

#[tokio::test]
async fn stateful_session_persists_edits_skips_none_diff_isolates_members_and_cleans_on_revoke() {
    let dir = std::env::temp_dir().join(format!("pb-session-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    init_repo(&dir);

    let runner = Arc::new(RecordingRunner::default());
    let host = Host::start_with(
        new_secret_key().unwrap(),
        true,
        HostConfig {
            repo: Some(dir.clone()),
            runner: Some(runner.clone()),
            limits: Limits::default(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let link_a = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let link_b = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let key_a = new_secret_key().unwrap();
    let id_a = key_a.public();
    let a = join_link(&link_a, key_a, true).await.unwrap();
    let b = join_link(&link_b, new_secret_key().unwrap(), true)
        .await
        .unwrap();

    // Step 1 (member A, keep=true, Delta): create file_a.txt -> patch returned.
    let r1 = a
        .run_task_session(
            "main",
            TaskSpec::new(&["sh", "-c", "echo step1 > file_a.txt && cat base.txt"]),
            true,
            true,
            SyncMode::Delta,
        )
        .await
        .unwrap();
    assert_eq!(r1.output.exit_code, Some(0));
    assert_eq!(r1.output.stdout.trim(), "hello");
    assert!(
        String::from_utf8_lossy(&r1.patch).contains("+step1"),
        "delta patch must carry file_a.txt: {}",
        String::from_utf8_lossy(&r1.patch)
    );

    // Step 2 (member A, keep=true, None): read-only check sees step1 WITHOUT replay, returns empty patch!
    let r2 = a
        .run_task_session(
            "main",
            TaskSpec::new(&["sh", "-c", "cat file_a.txt"]),
            true,
            true,
            SyncMode::None,
        )
        .await
        .unwrap();
    assert_eq!(r2.output.stdout.trim(), "step1");
    assert!(
        r2.patch.is_empty(),
        "SyncMode::None must skip host git diff"
    );

    // Step 3 (member A, keep=true, Full): append step2 and export authoritative full diff.
    let r3 = a
        .run_task_session(
            "main",
            TaskSpec::new(&["sh", "-c", "echo step2 >> file_a.txt"]),
            true,
            true,
            SyncMode::Full,
        )
        .await
        .unwrap();
    let p3 = String::from_utf8_lossy(&r3.patch);
    assert!(p3.contains("+step1") && p3.contains("+step2"), "{p3}");

    // Member B gets an isolated session worktree (must NOT see member A's file_a.txt).
    let rb = b
        .run_task_session(
            "main",
            TaskSpec::new(&["sh", "-c", "test ! -e file_a.txt && echo isolated"]),
            true,
            true,
            SyncMode::None,
        )
        .await
        .unwrap();
    assert_eq!(rb.output.stdout.trim(), "isolated");

    // Verify member A reused the exact same host worktree path across steps 1-3, distinct from B.
    {
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].3, calls[1].3);
        assert_eq!(calls[1].3, calls[2].3);
        assert_ne!(calls[0].3, calls[3].3);
        assert_eq!(calls[0].2, SyncMode::Delta);
        assert_eq!(calls[1].2, SyncMode::None);
        assert_eq!(calls[2].2, SyncMode::Full);
    }

    // Revoking member A must immediately tear down its warm sandbox and refuse subsequent tasks.
    assert!(host.revoke_member(&id_a));
    for _ in 0..40 {
        if runner
            .ended
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == &id_a.to_string())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        runner
            .ended
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == &id_a.to_string()),
        "revoking an idle member must immediately call end_warm"
    );
    assert!(a
        .run_task_session("main", TaskSpec::new(&["true"]), true, true, SyncMode::None,)
        .await
        .is_err());

    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
