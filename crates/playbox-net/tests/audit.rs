//! The audit log through a real host: complete, in order, tamper-evident, and free of secrets.

use playbox_net::{audit, join_link, new_secret_key, Client, Host, HostConfig, Link, Transport};
use playbox_runner::{Isolation, ProcessRunner, TaskSpec};
use serde_json::Value;
use std::{path::Path, process::Command, sync::Arc, time::Duration};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-auditit-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@e.com"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "i"]);
}

fn cfg(repo: std::path::PathBuf) -> HostConfig {
    HostConfig { repo: Some(repo), runner: Some(Arc::new(ProcessRunner)), min_isolation: Isolation::None, ..Default::default() }
}

fn events(path: &Path) -> Vec<Value> {
    audit::tail(path, 1000).unwrap()
}

fn kinds(path: &Path) -> Vec<String> {
    events(path).iter().map(|e| e["event"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn the_log_tells_the_story_and_keeps_the_secrets_out() {
    let (state, work) = (scratch("story-state"), scratch("story-repo"));
    repo(&work);
    let host = Host::open(&state, Transport::Local, cfg(work.clone())).await.unwrap();
    let log = host.audit_path().expect("a host with a state directory keeps an audit log");
    assert_eq!(log, state.join("audit.log"));

    let link = host.create_link(Duration::from_secs(60), 2).unwrap().encode();
    let invite = Link::parse(&link).unwrap().invite;
    let member = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let grant = member.grant().unwrap();

    // A stranger is refused; a spent invite is refused; an over-limit task is refused.
    let stranger = Client::connect(host.addr(), new_secret_key().unwrap(), true).await.unwrap();
    assert!(stranger.echo("x").await.is_err());
    assert!(join_link(&link, new_secret_key().unwrap(), true).await.is_ok(), "the second use of a two-use invite works");
    assert!(join_link(&link, new_secret_key().unwrap(), true).await.is_err(), "the third does not");

    member.pull("main").await.unwrap();
    // A command with a secret in its arguments.
    let secret = "hunter2-API-TOKEN-0xDEADBEEF";
    let spec = TaskSpec { timeout_secs: 20, ..TaskSpec::new(&["sh", "-c", &format!("echo using {secret} > out.txt; exit 3")]) };
    let (_, out, _) = member.run_task("main", spec).await.unwrap();
    assert_eq!(out.exit_code, Some(3));
    assert!(member.run_task("main", TaskSpec { cpus: 64.0, ..TaskSpec::new(&["true"]) }).await.is_err());
    let bundle = playbox_git::bundle_create(&work, "main").await.unwrap();
    member.push("main", &bundle).await.unwrap();
    assert!(host.revoke_member(&member.id()));

    // 1. The log verifies, with the key it names, and says so about itself.
    let summary = audit::verify(&log).unwrap();
    assert_eq!(summary.entries as usize, events(&log).len());
    assert_eq!(summary.host_key, events(&log)[0]["f"]["host_key"].as_str().unwrap());

    // 2. The events are there, in order.
    let k = kinds(&log);
    assert_eq!(k[0], "host_start");
    let pos = |name: &str| k.iter().position(|e| e == name).unwrap_or_else(|| panic!("no {name} event in {k:?}"));
    assert!(pos("join") < pos("pull") && pos("pull") < pos("task") && pos("task") < pos("push") && pos("push") < pos("revoke"), "{k:?}");
    let evs = events(&log);
    assert_eq!(evs.iter().filter(|e| e["event"] == "join" && e["f"]["ok"] == true).count(), 2);
    assert!(evs.iter().any(|e| e["event"] == "join" && e["f"]["ok"] == false), "the refused third join is logged");
    let refused: Vec<&Value> = evs.iter().filter(|e| e["event"] == "refused").collect();
    assert!(refused.iter().any(|e| e["f"]["code"] == -32040), "the stranger's denial is logged: {refused:?}");

    // 3. A task record says what ran, where, how it ended, and who asked, but not the secret.
    let task = evs.iter().find(|e| e["event"] == "task" && e["f"]["exit_code"] == 3).expect("the failing task is logged");
    assert_eq!(task["f"]["program"], "sh");
    assert_eq!(task["f"]["args"], 2);
    assert_eq!(task["f"]["cmd_hash"].as_str().unwrap().len(), 16);
    assert_eq!(task["f"]["state"], "Failed");
    assert_eq!(task["peer"].as_str().unwrap(), &member.id().to_string()[..16]);

    // 4. Nothing sensitive reached the file: no secret, no invite, no grant.
    let raw = std::fs::read_to_string(&log).unwrap();
    for (what, needle) in [("the command's secret", secret), ("the invite", invite.as_str()), ("the grant", grant.as_str())] {
        assert!(!raw.contains(needle), "{what} is in the audit log");
    }
    assert!(!raw.contains("out.txt"), "command text leaked into the log");
}

#[tokio::test]
async fn the_chain_continues_across_restarts_and_a_tampered_log_stops_the_host() {
    let (state, work) = (scratch("restart-state"), scratch("restart-repo"));
    repo(&work);
    let host = Host::open(&state, Transport::Local, cfg(work.clone())).await.unwrap();
    let log = host.audit_path().unwrap();
    let before = audit::verify(&log).unwrap().entries;
    host.shutdown().await.unwrap();

    let host = Host::open(&state, Transport::Local, cfg(work.clone())).await.unwrap();
    let after = audit::verify(&log).unwrap();
    assert!(after.entries > before, "the restart is recorded in the same chain");
    assert_eq!(kinds(&log).iter().filter(|e| *e == "host_start").count(), 2);
    host.shutdown().await.unwrap();

    // Someone edits the log. The host will not carry on writing into it as if nothing happened.
    let raw = std::fs::read_to_string(&log).unwrap();
    std::fs::write(&log, raw.replacen("\"replica\":false", "\"replica\":true", 1)).unwrap();
    assert!(audit::verify(&log).is_err());
    let err = Host::open(&state, Transport::Local, cfg(work)).await.err().expect("a tampered log must stop the host");
    assert!(format!("{err:#}").contains("audit"), "{err:#}");
}

#[tokio::test]
async fn an_ephemeral_host_keeps_no_log_unless_asked_and_can_log_to_a_chosen_file() {
    let work = scratch("eph-repo");
    repo(&work);
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg(work.clone())).await.unwrap();
    assert!(host.audit_path().is_none());
    drop(host);

    let file = scratch("eph-log").join("custom.log");
    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig { audit_log: Some(file.clone()), ..cfg(work) }).await.unwrap();
    assert_eq!(host.audit_path(), Some(file.clone()));
    assert!(audit::verify(&file).is_ok());
}
