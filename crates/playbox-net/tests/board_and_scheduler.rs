//! End-to-end integration tests for Playbox Board primitives:
//! - Signed Room Manifest (goal, /commands, schedules, budget, hillclimb template)
//! - Sender-bound `@handles`, `@mentions`, durable `inbox` / `ack` cursors across host restart
//! - Feed `/command` dispatch to sandboxed `Capability` + signed `command_receipt`
//! - Host Scheduler (periodic schedules + `on_push` triggers with cooldown)

use anyhow::Result;
use playbox_net::capabilities::Capability;
use playbox_net::manifest::{BudgetPolicy, ScheduleSpec, ScheduleTrigger};
use playbox_net::templates::TemplateKind;
use playbox_net::{new_secret_key, Client, Host, HostConfig, Link, Transport};
use playbox_runner::{Isolation, Runner, TaskOutput, TaskSpec};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("pbx-board-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

struct StubRunner;

impl Runner for StubRunner {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn isolation(&self) -> Isolation {
        Isolation::Container
    }

    fn run<'life0, 'life1, 'life2, 'async_trait>(
        &'life0 self,
        spec: &'life1 TaskSpec,
        work: &'life2 Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TaskOutput>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        Self: 'async_trait,
    {
        let joined = spec.cmd.join(" ");
        let has_eval = work.join("eval.sh").is_file();
        Box::pin(async move {
            Ok(TaskOutput {
                exit_code: Some(if has_eval { 0 } else { 2 }),
                stdout: format!("eval-ok(has_eval={has_eval}): {joined}\n"),
                stderr: String::new(),
                timed_out: false,
                policy_digest: None,
                secrets_used: vec![],
            })
        })
    }
}

#[tokio::test]
async fn manifest_handles_mentions_durable_inbox_and_slash_commands() {
    let dir = tmp_dir("e2e");
    let state_dir = dir.join("state");
    let repo_dir = dir.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    playbox_git::init(&repo_dir, "main").await.unwrap();
    std::fs::write(repo_dir.join("eval.sh"), "#!/bin/sh\necho SCORE=99.42\n").unwrap();
    for args in [
        &["config", "user.email", "t@t"][..],
        &["config", "user.name", "t"][..],
        &["add", "eval.sh"][..],
        &["commit", "-m", "add eval.sh"][..],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_dir)
            .args(args)
            .status()
            .unwrap()
            .success());
    }

    let eval_cap = Capability {
        name: "eval".into(),
        argv: vec!["./eval.sh".into(), "{ref}".into()],
        params: {
            let mut m = BTreeMap::new();
            m.insert(
                "ref".into(),
                playbox_net::capabilities::ParamRule {
                    allowed: None,
                    max_len: Some(64),
                },
            );
            m
        },
        approval: false,
        image: None,
        timeout_secs: Some(30),
        mem_mib: None,
        mount_repo: true,
    };

    let cfg = HostConfig {
        repo: Some(repo_dir.clone()),
        runner: Some(Arc::new(StubRunner)),
        template: Some(TemplateKind::Hillclimb.spec().id.to_string()),
        goal: Some("Minimize latency on main without regressing accuracy.".into()),
        capabilities: vec![eval_cap],
        schedules: vec![ScheduleSpec {
            id: "periodic-ping".into(),
            trigger: ScheduleTrigger::Every { secs: 1 },
            message: "@all periodic check-in".into(),
            min_interval_secs: 60,
        }],
        budget: BudgetPolicy {
            fast_model: Some("haiku".into()),
            frontier_model: Some("opus".into()),
            plateau_turns: 2,
            max_turns_per_hour: 10,
        },
        ..Default::default()
    };

    let host = Host::open(&state_dir, Transport::Local, cfg.clone())
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(300), 2)
        .unwrap()
        .encode();
    let parsed = Link::parse(&link).unwrap();

    // Two guests join the room.
    let key1 = new_secret_key().unwrap();
    let key2 = new_secret_key().unwrap();
    let c1 = Client::connect(parsed.addr.clone(), key1, Transport::Local)
        .await
        .unwrap();
    let _o1 = c1.join_details(&parsed.invite).await.unwrap();
    let c2 = Client::connect(parsed.addr.clone(), key2.clone(), Transport::Local)
        .await
        .unwrap();
    let _o2 = c2.join_details(&parsed.invite).await.unwrap();

    // 1. Signed Room Manifest verifies host signature & fields.
    let signed = c1.manifest().await.unwrap();
    assert_eq!(signed.manifest.template.as_deref(), Some("hillclimb"));
    assert_eq!(
        signed.manifest.goal.as_deref(),
        Some("Minimize latency on main without regressing accuracy.")
    );
    assert!(signed.manifest.commands.iter().any(|c| c.name == "eval"));
    assert_eq!(signed.manifest.budget.plateau_turns, 2);

    // Tampered manifest fails signature verification.
    let mut tampered = signed.clone();
    tampered.manifest.goal = Some("malicious goal".into());
    assert!(tampered.verify(&c1.host_id()).is_err());

    // 2. Handles are unique & sender-bound.
    assert_eq!(
        c1.register_handle("@alex-claude").await.unwrap(),
        "alex-claude"
    );
    assert!(c2.register_handle("alex-claude").await.is_err());
    assert_eq!(c2.register_handle("sam-codex").await.unwrap(), "sam-codex");

    // 3. Mentions & durable unread inbox/ack.
    let s1 = c1
        .say("hey @sam-codex check this diff", Some("alex"))
        .await
        .unwrap();
    let s2 = c1.say("unrelated chatter", Some("alex")).await.unwrap();
    assert!(s2 > s1);

    let inbox_mentions = c2.inbox(true, 50, 0).await.unwrap();
    assert!(inbox_mentions.items.iter().any(|i| i.seq == s1));
    assert!(!inbox_mentions.items.iter().any(|i| i.seq == s2));

    // Ack advances cursor; subsequent inbox returns empty.
    let acked = c2.ack(inbox_mentions.next).await.unwrap();
    assert!(acked >= s2);
    let empty = c2.inbox(false, 50, 0).await.unwrap();
    assert!(empty.items.is_empty());

    // 4. Feed `/eval ref=main` dispatches sandboxed capability with staged repo snapshot & posts signed receipt.
    c1.say("/eval ref=main", Some("alex")).await.unwrap();
    let mut saw_receipt = false;
    for _ in 0..40 {
        let f = c1.inbox(false, 50, 0).await.unwrap();
        if f.items.iter().any(|it| {
            it.kind == "event"
                && it
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("event"))
                    .and_then(|v| v.as_str())
                    == Some("command_receipt")
                && it.text.contains("/eval")
                && it.text.contains("exit=0")
                && it.text.contains("has_eval=true")
        }) {
            saw_receipt = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        saw_receipt,
        "expected host-originated signed command_receipt with staged repo in requester's inbox"
    );

    // 5. Periodic schedule fires `@all periodic check-in` (cooldown prevents spam).
    let mut saw_sched = false;
    for _ in 0..40 {
        let f = c1.feed(0).await.unwrap();
        if f.items
            .iter()
            .any(|it| it.kind == "say" && it.text == "@all periodic check-in")
        {
            saw_sched = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(saw_sched, "expected periodic schedule message in feed");

    // 6. Durable cursor survives host restart.
    let c2_grant = c2.grant().expect("grant after join").to_string();
    c1.close().await;
    c2.close().await;
    host.shutdown().await.unwrap();

    let host2 = Host::open(&state_dir, Transport::Local, cfg).await.unwrap();
    let c2_re = Client::resume(host2.addr(), key2, Transport::Local, c2_grant)
        .await
        .unwrap();
    let post_restart = c2_re.inbox(false, 50, 0).await.unwrap();
    // Previously acked `s1`/`s2` are NOT re-delivered after restart!
    assert!(!post_restart.items.iter().any(|i| i.seq <= s2));

    c2_re.close().await;
    host2.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
