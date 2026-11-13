//! Secrets reach a Docker task at /run/secrets and nowhere else: not in the workspace, not in the output record, and the staging
//! directory is gone afterwards. Needs a running Docker daemon with `alpine:3`; skips (with a note) without one.

use playbox_runner::{secrets::SecretStore, DockerRunner, Runner, SandboxPolicy, TaskSpec};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

fn docker_ready() -> bool {
    std::process::Command::new("docker")
        .args(["image", "inspect", "alpine:3"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn a_task_reads_its_secret_and_nothing_leaks_into_the_workspace_or_the_record() {
    if !docker_ready() {
        eprintln!("SKIP: docker with alpine:3 is not available");
        return;
    }
    let base = std::env::temp_dir().join(format!("playbox-secrets-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let store = base.join("store");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&store).unwrap();
    std::fs::write(store.join("API_KEY"), "s3cr3t-value").unwrap();
    let ws = base.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o777)).unwrap();

    let runner = DockerRunner::new(SandboxPolicy::default()).with_secrets(SecretStore::new(&store));
    let spec = TaskSpec { secrets: vec!["API_KEY".into()], ..TaskSpec::new(&["sh", "-c", "cat /run/secrets/API_KEY; ls /run/secrets; touch /run/secrets/x 2>&1 | head -c 80"]) };
    let out = runner.run(&spec, &ws).await.unwrap();
    assert!(out.stdout.contains("s3cr3t-value"), "{out:?}");
    assert!(out.stdout.contains("API_KEY"));
    // Read-only inside the task.
    assert!(out.stdout.to_lowercase().contains("read-only") || out.stderr.to_lowercase().contains("read-only"), "{out:?}");
    // The record names the secret and hashes it; it never carries the value.
    assert_eq!(out.secrets_used.len(), 1);
    assert_eq!(out.secrets_used[0].name, "API_KEY");
    assert_eq!(out.secrets_used[0].blake3, blake3::hash(b"s3cr3t-value").to_hex().to_string());
    let record = serde_json::to_string(&out.secrets_used).unwrap();
    assert!(!record.contains("s3cr3t-value"));
    // Nothing in the workspace.
    assert_eq!(std::fs::read_dir(&ws).unwrap().count(), 0);
    // No staging directory left behind.
    let left = std::fs::read_dir(std::env::temp_dir()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("secrets-")).count();
    assert_eq!(left, 0, "staging directories were left in the temp directory");

    // Unknown secrets and a task with none.
    let missing = TaskSpec { secrets: vec!["NOPE".into()], ..TaskSpec::new(&["true"]) };
    assert!(runner.run(&missing, &ws).await.is_err());
    let none = runner.run(&TaskSpec::new(&["sh", "-c", "ls /run/secrets 2>&1 | head -c 40; true"]), &ws).await.unwrap();
    assert!(!none.stdout.contains("API_KEY"));
    let _ = std::fs::remove_dir_all(&base);
}
