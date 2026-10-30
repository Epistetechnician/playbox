//! Abuse limits and revocation of running tasks. The in-process runner stands in for a sandbox; what is
//! under test is the host's accounting and cancellation, not isolation.

use playbox_net::{join_link, limits::AbuseLimits, new_secret_key, Client, Host, HostConfig};
use playbox_runner::{Isolation, ProcessRunner, TaskSpec};
use std::{
    path::Path,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn scratch(name: &str) -> std::path::PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("playbox-abuse-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn repo() -> std::path::PathBuf {
    let dir = scratch("repo");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@e.com"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "i"]);
    dir
}

async fn host(abuse: AbuseLimits) -> Host {
    let cfg = HostConfig {
        repo: Some(repo()),
        runner: Some(Arc::new(ProcessRunner)),
        min_isolation: Isolation::None,
        abuse,
        ..Default::default()
    };
    Host::start_with(new_secret_key().unwrap(), true, cfg)
        .await
        .unwrap()
}

async fn join(host: &Host) -> Arc<Client> {
    let link = host
        .create_link(Duration::from_secs(60), 5)
        .unwrap()
        .encode();
    Arc::new(
        join_link(&link, new_secret_key().unwrap(), true)
            .await
            .unwrap(),
    )
}

fn sh(script: &str) -> TaskSpec {
    TaskSpec {
        timeout_secs: 30,
        ..TaskSpec::new(&["sh", "-c", script])
    }
}

/// The host serves one request at a time per connection, so a member only runs tasks in parallel by
/// opening more connections with the same key. That is what the per-member limit has to stop.
#[tokio::test]
async fn a_member_cannot_run_more_tasks_at_once_than_allowed_even_over_many_connections() {
    let h = host(AbuseLimits {
        max_concurrent_per_member: 1,
        ..Default::default()
    })
    .await;
    let secret = new_secret_key().unwrap();
    let link = h.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let c1 = Arc::new(join_link(&link, secret.clone(), true).await.unwrap());
    let c2 = Arc::new(Client::connect(h.addr(), secret, true).await.unwrap());
    c2.set_grant(c1.grant().expect("joined"));
    let first = Arc::clone(&c1);
    let slow = tokio::spawn(async move { first.run_task("main", sh("sleep 2")).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let err = c2.run_task("main", sh("true")).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("already have 1 tasks running"),
        "{err:#}"
    );
    slow.await.unwrap().unwrap();
    c2.run_task("main", sh("true"))
        .await
        .expect("the slot is free again once the first task ends");
}

#[tokio::test]
async fn one_members_load_does_not_use_up_anothers_slot() {
    let h = host(AbuseLimits {
        max_concurrent_per_member: 1,
        ..Default::default()
    })
    .await;
    let (a, b) = (join(&h).await, join(&h).await);
    let a2 = Arc::clone(&a);
    let slow = tokio::spawn(async move { a2.run_task("main", sh("sleep 2")).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    b.run_task("main", sh("true"))
        .await
        .expect("b has its own slot");
    slow.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_rate_limit_and_the_room_budget_stop_floods() {
    let h = host(AbuseLimits {
        max_tasks_per_window: 2,
        task_budget: 3,
        max_concurrent_per_member: 10,
        ..Default::default()
    })
    .await;
    let (a, b) = (join(&h).await, join(&h).await);
    a.run_task("main", sh("true")).await.unwrap();
    a.run_task("main", sh("true")).await.unwrap();
    let e = a.run_task("main", sh("true")).await.unwrap_err();
    assert!(format!("{e:#}").contains("rate limit"), "{e:#}");
    b.run_task("main", sh("true")).await.unwrap(); // third task in the room
    let e = b.run_task("main", sh("true")).await.unwrap_err();
    assert!(format!("{e:#}").contains("budget"), "{e:#}");
}

#[tokio::test]
async fn the_room_stops_admitting_members_at_the_cap() {
    let h = host(AbuseLimits {
        max_members: 2,
        ..Default::default()
    })
    .await;
    let link = h.create_link(Duration::from_secs(60), 5).unwrap().encode();
    join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let e = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .err()
        .expect("third member is refused");
    assert!(format!("{e:#}").contains("room is full"), "{e:#}");
}

#[tokio::test]
async fn revoking_a_member_stops_the_task_they_have_running() {
    let h = host(AbuseLimits::default()).await;
    let c = join(&h).await;
    let marker = scratch("marker").join("SURVIVED");
    let script = format!("sleep 4; touch {}", marker.display());
    let me = c.id();
    let c2 = Arc::clone(&c);
    let t0 = Instant::now();
    let running = tokio::spawn(async move { c2.run_task("main", sh(&script)).await });
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(h.revoke_member(&me));
    let res = running.await.unwrap();
    let e = res.expect_err("the running task is cancelled, not returned");
    assert!(format!("{e:#}").contains("revoked"), "{e:#}");
    assert!(
        t0.elapsed() < Duration::from_secs(3),
        "cancelled promptly, took {:?}",
        t0.elapsed()
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        !marker.exists(),
        "the task's process kept running after revocation"
    );
}
