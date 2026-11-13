//! Authorization: forged, stolen, attenuated, expired and revoked grants.
//! Uses `ProcessRunner` for task limits so these tests do not need Docker; the command is harmless.

use playbox_net::{join_link, new_secret_key, Client, Host, HostConfig, Link};
use playbox_runner::{Limits, ProcessRunner, TaskSpec};
use std::{path::Path, process::Command, sync::Arc, time::Duration};

fn sh(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("playbox-grants-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    sh(&dir, &["init", "-q", "-b", "main"]);
    sh(&dir, &["config", "user.email", "t@example.com"]);
    sh(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    sh(&dir, &["add", "-A"]);
    sh(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

async fn room(name: &str, cfg: impl FnOnce(&mut HostConfig)) -> (Host, String) {
    let mut c = HostConfig {
        min_isolation: playbox_runner::Isolation::None,
        repo: Some(repo(name)),
        runner: Some(Arc::new(ProcessRunner)),
        limits: Limits {
            cpus: 2.0,
            mem_mib: 1024,
            timeout_secs: 60,
            network: false,
            allowed_hosts: vec![],
            allowed_secrets: vec![],
        },
        ..Default::default()
    };
    cfg(&mut c);
    let host = Host::start_with(new_secret_key().unwrap(), true, c)
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 5)
        .unwrap()
        .encode();
    (host, link)
}

fn msg(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tokio::test]
async fn invite_from_another_room_is_forged() {
    let (host_a, _) = room("forge-a", |_| {}).await;
    let (_host_b, link_b) = room("forge-b", |_| {}).await;
    // B's invite, but dialed to A: signed by B's root key, so A refuses it.
    let mut link = Link::parse(&link_b).unwrap();
    link.addr = host_a.addr();
    let err = join_link(&link.encode(), new_secret_key().unwrap(), true)
        .await
        .err()
        .expect("refused");
    assert!(msg(err).contains("not valid for this room"));
}

// REQ: GRANT-1
#[tokio::test]
async fn stolen_grant_is_useless_to_another_key() {
    let (host, link) = room("steal", |_| {}).await;
    let alice = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    assert_eq!(alice.echo("ok").await.unwrap(), "ok");

    let mallory = Client::connect(host.addr(), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    mallory.set_grant(alice.grant().unwrap());
    let err = mallory.echo("steal").await.unwrap_err();
    assert!(msg(err).contains("not authorized"));
}

// REQ: GRANT-2
#[tokio::test]
async fn attenuated_grant_cannot_do_more() {
    let (host, link) = room("atten", |_| {}).await;
    let c = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    c.pull("main").await.unwrap();

    c.attenuate(r#"check if operation("echo");"#).unwrap();
    assert_eq!(c.echo("still fine").await.unwrap(), "still fine");
    let err = c.pull("main").await.unwrap_err();
    assert!(
        msg(err).contains("not authorized"),
        "pull must be refused after attenuation"
    );
    drop(host);
}

#[tokio::test]
async fn task_limits_are_enforced_not_clamped() {
    let (_host, link) = room("limits", |_| {}).await;
    let c = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();

    let ok = TaskSpec {
        timeout_secs: 20,
        ..TaskSpec::new(&["sh", "-c", "echo hi"])
    };
    let (_, out, _) = c.run_task("main", ok).await.unwrap();
    assert_eq!(out.stdout, "hi\n");

    // Over the host's cpu cap (2.0), over network policy, and over the memory cap: all refused.
    for bad in [
        TaskSpec {
            cpus: 4.0,
            ..TaskSpec::new(&["true"])
        },
        TaskSpec {
            network: true,
            ..TaskSpec::new(&["true"])
        },
        TaskSpec {
            mem_mib: 4096,
            ..TaskSpec::new(&["true"])
        },
    ] {
        let err = c.run_task("main", bad).await.unwrap_err();
        assert!(msg(err).contains("not authorized"));
    }

    // A member narrows their own timeout, then cannot exceed it.
    c.attenuate("check if req_timeout($t), $t <= 5;").unwrap();
    let long = TaskSpec {
        timeout_secs: 30,
        ..TaskSpec::new(&["true"])
    };
    assert!(c.run_task("main", long).await.is_err());
    let short = TaskSpec {
        timeout_secs: 5,
        ..TaskSpec::new(&["true"])
    };
    assert!(c.run_task("main", short).await.is_ok());

    // And can restrict which program may run.
    c.attenuate(r#"check if req_cmd("sh");"#).unwrap();
    assert!(c
        .run_task(
            "main",
            TaskSpec {
                timeout_secs: 5,
                ..TaskSpec::new(&["true"])
            }
        )
        .await
        .is_err());
    assert!(c
        .run_task(
            "main",
            TaskSpec {
                timeout_secs: 5,
                ..TaskSpec::new(&["sh", "-c", "true"])
            }
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn expired_grant_is_refused() {
    let (_host, link) = room("expiry", |c| c.grant_ttl = Some(Duration::ZERO)).await;
    let c = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let err = c.echo("late").await.unwrap_err();
    assert!(msg(err).contains("not authorized"));
}

// REQ: GRANT-3
#[tokio::test]
async fn revoked_member_loses_access_and_cannot_rejoin() {
    let (host, link) = room("revoke", |_| {}).await;
    let key = new_secret_key().unwrap();
    let c = join_link(&link, key.clone(), true).await.unwrap();
    assert_eq!(c.echo("before").await.unwrap(), "before");

    // Attenuated copies die with the parent grant.
    c.attenuate(r#"check if operation("echo");"#).unwrap();
    assert!(host.revoke_member(&c.id()));

    let err = c.echo("after").await.unwrap_err();
    assert!(msg(err).contains("revoked"));

    // A fresh invite does not let the same key back in.
    let link2 = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let err = join_link(&link2, key, true).await.err().expect("banned");
    assert!(msg(err).contains("banned"));

    // Other members are unaffected.
    let other = join_link(&link2, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    assert_eq!(other.echo("fine").await.unwrap(), "fine");
}

// REQ: GRANT-4
#[tokio::test]
async fn invite_cannot_be_used_as_a_grant() {
    let (host, link) = room("invite-as-grant", |_| {}).await;
    let c = Client::connect(host.addr(), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    c.set_grant(Link::parse(&link).unwrap().invite);
    let err = c.echo("x").await.unwrap_err();
    assert!(msg(err).contains("not authorized"));
}

#[tokio::test]
async fn host_lists_are_checked_against_the_grant_and_the_runner() {
    let (_host, link) = room("hosts", |c| {
        c.limits.allowed_hosts = vec!["api.github.com".into()];
    })
    .await;
    let c = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();

    // A host the grant does not list is refused by authorization, even alongside a listed one.
    for hosts in [
        vec!["evil.example"],
        vec!["api.github.com", "evil.example"],
        vec!["api.github.com:8443"],
    ] {
        let spec = TaskSpec {
            allowed_hosts: hosts.iter().map(std::string::ToString::to_string).collect(),
            ..TaskSpec::new(&["true"])
        };
        assert!(msg(c.run_task("main", spec).await.unwrap_err()).contains("not authorized"));
    }
    // A listed host passes the grant, but ProcessRunner cannot enforce a host list, so the runner refuses
    // rather than quietly running with the network wide open.
    let spec = TaskSpec {
        allowed_hosts: vec!["api.github.com".into()],
        ..TaskSpec::new(&["true"])
    };
    assert!(msg(c.run_task("main", spec).await.unwrap_err())
        .contains("cannot limit a task to specific hosts"));

    // A member can narrow a grant offline to no hosts at all.
    // ("" is the always-present placeholder every request carries; see grants.rs.)
    c.attenuate(r#"check all req_host($h), [""].contains($h);"#)
        .unwrap();
    let spec = TaskSpec {
        allowed_hosts: vec!["api.github.com".into()],
        ..TaskSpec::new(&["true"])
    };
    assert!(msg(c.run_task("main", spec).await.unwrap_err()).contains("not authorized"));
}

#[tokio::test]
async fn no_hosts_in_the_grant_means_no_host_list_is_allowed() {
    let (_host, link) = room("nohosts", |_| {}).await;
    let c = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let spec = TaskSpec {
        allowed_hosts: vec!["example.com".into()],
        ..TaskSpec::new(&["true"])
    };
    let e = msg(c.run_task("main", spec).await.unwrap_err());
    assert!(e.contains("not authorized"), "{e}");
}

#[tokio::test]
async fn secrets_are_checked_against_the_grant_and_the_runner() {
    let (_host, link) = room("secrets", |c| {
        c.limits.allowed_secrets = vec!["API_KEY".into()];
    })
    .await;
    let c = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let ask = |names: &[&str]| TaskSpec {
        secrets: names.iter().map(|s| (*s).to_string()).collect(),
        ..TaskSpec::new(&["true"])
    };
    // A secret the grant does not list is refused by authorization, even alongside a listed one.
    for names in [&["OTHER"][..], &["API_KEY", "OTHER"]] {
        assert!(msg(c.run_task("main", ask(names)).await.unwrap_err()).contains("not authorized"), "{names:?}");
    }
    // A listed one passes the grant; the process runner cannot hand secrets over, so it refuses rather than run without them.
    assert!(msg(c.run_task("main", ask(&["API_KEY"])).await.unwrap_err()).contains("cannot hand secrets"));
    // A host list naming a secret passes no further than the runner, which cannot enforce host lists here and refuses.
    let spec = TaskSpec { allowed_hosts: vec!["secret/API_KEY".into()], ..TaskSpec::new(&["true"]) };
    assert!(msg(c.run_task("main", spec).await.unwrap_err()).contains("cannot limit a task to specific hosts"));
}
