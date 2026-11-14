//! Capabilities end to end: a member calls what the owner offers, once, within bounds, with the owner's approval where required,
//! and the result is in the audit log. Uses `ProcessRunner` (no isolation) so the tests need no Docker; the commands are harmless.

use playbox_net::capabilities::{Capability, ParamRule, State};
use playbox_net::{join_link, new_secret_key, Client, Host, HostConfig};
use playbox_runner::{Isolation, Limits, ProcessRunner};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-caps-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

async fn room(name: &str, caps: Vec<Capability>) -> (Host, Client, std::path::PathBuf) {
    let dir = tmp(name);
    let log = dir.join("audit.log");
    let cfg = HostConfig {
        min_isolation: Isolation::None,
        runner: Some(Arc::new(ProcessRunner)),
        limits: Limits { allowed_secrets: vec![], ..Default::default() },
        audit_log: Some(log.clone()),
        capabilities: caps,
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 5).unwrap().encode();
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    (host, c, log)
}

fn greet(approval: bool) -> Capability {
    Capability {
        name: "greet".into(),
        argv: vec!["echo".into(), "hello".into(), "{who}".into()],
        params: BTreeMap::from([("who".into(), ParamRule { allowed: Some(vec!["world".into(), "room".into()]), max_len: None })]),
        approval,
        image: None,
        timeout_secs: Some(10),
        mem_mib: None,
    }
}

// REQ: CAP-6
#[tokio::test]
async fn a_call_runs_in_the_sandbox_and_its_result_hash_is_in_the_audit_log() {
    let (_h, c, log) = room("basic", vec![greet(false)]).await;
    let offer = c.capabilities().await.unwrap();
    assert_eq!(offer[0]["name"], "greet");
    assert!(!offer.to_string().contains("echo"), "the command is the owner's, not shown to members");

    let call = c.call_capability_wait("greet", args(&[("who", "world")]), "c1", Duration::from_secs(20)).await.unwrap();
    assert_eq!(call.state, State::Done);
    let o = call.outcome.unwrap();
    assert_eq!((o.exit_code, o.stdout.trim()), (Some(0), "hello world"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains(&o.result_hash), "the audit log must hold the result hash");
    assert!(text.contains("capability_call") && text.contains("capability_result"));
}

// REQ: CAP-3
#[tokio::test]
async fn a_repeated_call_id_runs_once() {
    let marker = tmp("once").join("count");
    let cap = Capability {
        name: "count".into(),
        argv: vec!["sh".into(), "-c".into(), format!("echo x >> {}; echo ok", marker.display())],
        params: BTreeMap::new(),
        approval: false,
        image: None,
        timeout_secs: Some(10),
        mem_mib: None,
    };
    let (_h, c, _) = room("once", vec![cap]).await;
    for _ in 0..4 {
        c.call_capability("count", BTreeMap::new(), "same").await.unwrap();
    }
    let done = c.call_capability_wait("count", BTreeMap::new(), "same", Duration::from_secs(20)).await.unwrap();
    assert_eq!(done.state, State::Done);
    // A late retry of a finished call returns it and runs nothing.
    c.call_capability("count", BTreeMap::new(), "same").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1, "the command must have run exactly once");
}

// REQ: CAP-1, CAP-2
#[tokio::test]
async fn bounds_grants_and_id_reuse_are_enforced() {
    let (_h, c, _) = room("bounds", vec![greet(false)]).await;
    let refused = |e: anyhow::Error| format!("{e:#}");
    assert!(refused(c.call_capability("greet", args(&[("who", "mallory")]), "b1").await.unwrap_err()).contains("allows"));
    assert!(refused(c.call_capability("greet", args(&[]), "b2").await.unwrap_err()).contains("missing"));
    assert!(refused(c.call_capability("greet", args(&[("who", "world"), ("x", "y")]), "b3").await.unwrap_err()).contains("takes no argument"));
    assert!(refused(c.call_capability("nope", args(&[]), "b4").await.unwrap_err()).contains("not authorized"));
    assert!(refused(c.call_capability("greet", args(&[("who", "world")]), "bad id!").await.unwrap_err()).contains("call_id"));
    c.call_capability("greet", args(&[("who", "world")]), "b5").await.unwrap();
    assert!(refused(c.call_capability("greet", args(&[("who", "room")]), "b5").await.unwrap_err()).contains("different request"));
    assert!(refused(c.poll_capability("nobody-has-this").await.unwrap_err()).contains("no such call"));
}

// REQ: CAP-1, GRANT-2
#[tokio::test]
async fn a_member_can_narrow_their_grant_so_a_capability_is_no_longer_allowed() {
    let (_h, c, _) = room("narrow", vec![greet(false)]).await;
    c.attenuate(r#"check all req_host($h), [""].contains($h);"#).unwrap();
    let e = format!("{:#}", c.call_capability("greet", args(&[("who", "world")]), "n1").await.unwrap_err());
    assert!(e.contains("not authorized"), "{e}");
}

// REQ: CAP-4
#[tokio::test]
async fn a_call_that_needs_approval_waits_for_the_owner() {
    let (h, c, log) = room("approval", vec![greet(true)]).await;
    let call = c.call_capability("greet", args(&[("who", "world")]), "a1").await.unwrap();
    assert_eq!(call.state, State::AwaitingApproval);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(c.poll_capability("a1").await.unwrap().state, State::AwaitingApproval, "nothing runs before approval");
    assert_eq!(h.pending_calls().len(), 1);

    h.decide_call("a1", true).unwrap();
    let done = c.call_capability_wait("greet", args(&[("who", "world")]), "a1", Duration::from_secs(20)).await.unwrap();
    assert_eq!(done.outcome.unwrap().stdout.trim(), "hello world");

    c.call_capability("greet", args(&[("who", "room")]), "a2").await.unwrap();
    h.decide_call("a2", false).unwrap();
    assert_eq!(c.poll_capability("a2").await.unwrap().state, State::Denied);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.matches("capability_decision").count() == 2);
    assert!(h.decide_call("a2", true).is_err(), "a decided call cannot be decided again");
}

// REQ: CAP-5
#[tokio::test]
async fn a_member_revoked_while_waiting_for_approval_gets_no_last_run() {
    let dir = tmp("revoked-waiting");
    let marker = dir.join("ran");
    let cap = Capability {
        name: "touch".into(),
        argv: vec!["sh".into(), "-c".into(), format!("touch {}", marker.display())],
        params: BTreeMap::new(),
        approval: true,
        image: None,
        timeout_secs: Some(10),
        mem_mib: None,
    };
    let (h, c, log) = room("revoked-waiting", vec![cap]).await;
    c.call_capability("touch", BTreeMap::new(), "w1").await.unwrap();
    assert_eq!(h.pending_calls().len(), 1);

    assert!(h.revoke_member(&c.id()));
    let err = h.decide_call("w1", true).unwrap_err();
    assert!(format!("{err:#}").contains("not run"), "{err:#}");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!marker.exists(), "the owner's yes does not run a revoked member's call");
    assert!(h.pending_calls().is_empty());
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("capability_refused"), "the refusal is in the audit log");
}

// REQ: CAP-5
#[tokio::test]
async fn a_member_revoked_while_a_call_runs_stops_it() {
    let dir = tmp("revoked-running");
    let marker = dir.join("finished");
    let cap = Capability {
        name: "slow".into(),
        argv: vec!["sh".into(), "-c".into(), format!("sleep 6; touch {}", marker.display())],
        params: BTreeMap::new(),
        approval: false,
        image: None,
        timeout_secs: Some(20),
        mem_mib: None,
    };
    let (h, c, log) = room("revoked-running", vec![cap]).await;
    c.call_capability("slow", BTreeMap::new(), "r1").await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(h.revoke_member(&c.id()));
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(!marker.exists(), "a revoked member's running call is stopped, not left to finish");
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("capability_cancelled"), "{text}");
}
