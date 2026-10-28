//! The A2A binding: agent card, plain A2A messages, tracked tasks, standard errors.

use a2a::{methods, TaskState};
use playbox_net::{join_link, new_secret_key, wire, Client, Host, HostConfig};
use playbox_runner::{Limits, ProcessRunner, TaskSpec};
use serde_json::json;
use std::{path::Path, process::Command, sync::Arc, time::Duration};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn repo(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("playbox-a2a-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

async fn room(name: &str, with_repo: bool, with_runner: bool) -> (Host, String) {
    let cfg = HostConfig { min_isolation: playbox_runner::Isolation::None,
        repo: with_repo.then(|| repo(name)),
        runner: with_runner.then(|| Arc::new(ProcessRunner) as _),
        limits: Limits { cpus: 2.0, mem_mib: 1024, timeout_secs: 60, network: false },
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 5).unwrap().encode();
    (host, link)
}

fn msg(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tokio::test]
async fn agent_card_is_public_and_describes_the_room() {
    let (host, link) = room("card", true, true).await;
    // No join needed: the card is the public face of the room.
    let c = Client::connect(host.addr(), new_secret_key().unwrap(), true).await.unwrap();
    let card = c.agent_card().await.unwrap();
    assert_eq!(card.supported_interfaces[0].protocol_binding, wire::BINDING);
    assert_eq!(card.supported_interfaces[0].url, format!("iroh://{}", host.addr().id));
    let ids: Vec<_> = card.skills.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["echo", "pull", "push", "run"]);
    assert_eq!(card.capabilities.streaming, Some(false));
    assert!(card.security_schemes.unwrap().contains_key("grant"));
    drop(link);

    // A room without a runner does not advertise `run`.
    let (host2, _) = room("card2", true, false).await;
    let c2 = Client::connect(host2.addr(), new_secret_key().unwrap(), true).await.unwrap();
    let ids2: Vec<_> = c2.agent_card().await.unwrap().skills.into_iter().map(|s| s.id).collect();
    assert_eq!(ids2, ["echo", "pull", "push"]);
}

#[tokio::test]
async fn plain_a2a_messages_get_a_helpful_reply() {
    let (_host, link) = room("plain", false, false).await;
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    // A generic A2A client knows nothing about Playbox operations: it just sends text.
    let req = json!({"message": {"messageId": "m1", "role": "ROLE_USER", "parts": [{"text": "hello there"}]}});
    let res = c.call(methods::SEND_MESSAGE, req).await.unwrap();
    let text = res["message"]["parts"][0]["text"].as_str().unwrap();
    assert!(text.contains("Playbox room"), "{text}");
    assert_eq!(res["message"]["role"], "ROLE_AGENT");
}

#[tokio::test]
async fn tasks_are_tracked_listed_and_private_to_their_owner() {
    let (host, link) = room("tasks", true, true).await;
    let alice = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let bob = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();

    let spec = TaskSpec { timeout_secs: 20, ..TaskSpec::new(&["sh", "-c", "echo hi > out.txt; echo done"]) };
    let (_, out, patch) = alice.run_task("main", spec).await.unwrap();
    assert_eq!(out.stdout, "done\n");
    assert!(String::from_utf8_lossy(&patch).contains("out.txt"));

    let tasks = alice.list_tasks().await.unwrap();
    assert_eq!(tasks.len(), 1);
    let t = alice.get_task(&tasks[0].id).await.unwrap();
    assert_eq!(t.status.state, TaskState::Completed);
    assert_eq!(wire::meta(&t, "exitCode"), Some(&json!(0)));
    assert_eq!(wire::artifact_text(&t, "stdout").as_deref(), Some("done\n"));
    // The stored record drops the patch bytes but says how big they were.
    assert!(wire::artifact_bytes(&t, "patch").is_none());
    let p = &wire::artifact(&t, "patch").unwrap().parts[0];
    assert!(p.metadata.as_ref().unwrap()["bytesOmitted"].as_u64().unwrap() > 0);

    // Bob cannot see Alice's task, and sees none of his own yet.
    let e = bob.get_task(&t.id).await.unwrap_err();
    assert!(msg(e).to_lowercase().contains("not found"));
    assert!(bob.list_tasks().await.unwrap().is_empty());

    // Finished tasks cannot be cancelled; unknown ones are not found.
    let e = alice.call(methods::CANCEL_TASK, json!({"id": t.id})).await.unwrap_err();
    assert!(msg(e).to_lowercase().contains("cancel"));
    drop(host);
}

#[tokio::test]
async fn a_failing_command_is_a_failed_task_with_its_output() {
    let (_host, link) = room("failing", true, true).await;
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let spec = TaskSpec { timeout_secs: 20, ..TaskSpec::new(&["sh", "-c", "echo oops >&2; exit 3"]) };
    let (_, out, _) = c.run_task("main", spec).await.unwrap();
    assert_eq!(out.exit_code, Some(3));
    assert_eq!(out.stderr, "oops\n");
    let t = &c.list_tasks().await.unwrap()[0];
    assert_eq!(t.status.state, TaskState::Failed);
    assert_eq!(wire::status_text(t), "exit code 3");
}

#[tokio::test]
async fn standard_a2a_errors() {
    let (_host, link) = room("errors", true, false).await;
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let e = msg(c.call(methods::SEND_STREAMING_MESSAGE, json!({})).await.unwrap_err());
    assert!(e.to_lowercase().contains("streaming"), "{e}");
    let e = msg(c.call("NoSuchMethod", json!({})).await.unwrap_err());
    assert!(e.contains("method not found"), "{e}");
    let e = msg(c.call(methods::SEND_MESSAGE, json!({"nonsense": true})).await.unwrap_err());
    assert!(e.to_lowercase().contains("invalid") || e.to_lowercase().contains("missing"), "{e}");
    let e = msg(c.call(methods::GET_EXTENDED_AGENT_CARD, json!({})).await.unwrap_err());
    assert!(e.to_lowercase().contains("extended"), "{e}");
    // Unknown operations inside a well-formed message are invalid params, not a crash.
    let bad = json!({"message": {"messageId": "m", "role": "ROLE_USER", "parts": [{"data": {"playbox": {"op": "format_disk"}}}]}});
    assert!(c.call(methods::SEND_MESSAGE, bad).await.is_err());
}

#[tokio::test]
async fn requests_without_a_grant_are_refused_but_the_card_is_not() {
    let (host, _) = room("nogrant", false, false).await;
    let c = Client::connect(host.addr(), new_secret_key().unwrap(), true).await.unwrap();
    assert!(c.agent_card().await.is_ok());
    let e = msg(c.echo("x").await.unwrap_err());
    assert!(e.contains("no grant presented"), "{e}");
    let e = msg(c.list_tasks().await.unwrap_err());
    assert!(e.contains("no grant presented"), "{e}");
}

#[tokio::test]
async fn task_queries_need_the_tasks_right() {
    let (_host, link) = room("tasksright", false, false).await;
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    assert!(c.list_tasks().await.is_ok());
    c.attenuate(r#"check if operation("echo");"#).unwrap();
    assert_eq!(c.echo("fine").await.unwrap(), "fine");
    let e = msg(c.list_tasks().await.unwrap_err());
    assert!(e.contains("not authorized"), "{e}");
}

#[tokio::test]
async fn a_large_bundle_survives_the_round_trip() {
    let (_host, link) = room("big", true, false).await;
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();

    // A client repo with ~6 MB of incompressible-ish content on a branch.
    let work = repo("bigclient");
    let mut blob = Vec::with_capacity(6 << 20);
    let mut x: u64 = 0x9E3779B97F4A7C15;
    while blob.len() < (6 << 20) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        blob.extend_from_slice(&x.to_le_bytes());
    }
    std::fs::write(work.join("blob.bin"), &blob).unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "blob"]);
    let bundle = playbox_git::bundle_create(&work, "main").await.unwrap();
    assert!(bundle.len() > 5 << 20);
    let reference = c.push("main", &bundle).await.unwrap();
    assert!(reference.starts_with("refs/playbox/members/"));

    // And back down the other way.
    let (_, pulled) = c.pull("main").await.unwrap();
    assert!(!pulled.is_empty());
}
