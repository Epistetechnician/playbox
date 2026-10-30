//! A member's push waits for the host. Nothing a member sends moves `main` until the host accepts it.

use playbox_net::{join_link, new_secret_key, review, Host, HostConfig};
use std::{path::Path, process::Command, time::Duration};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn scratch(name: &str) -> std::path::PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("playbox-review-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn init(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@e.com"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

/// Host with a repo, one member with a local clone that has a commit on top of main.
async fn setup() -> (
    Host,
    std::path::PathBuf,
    playbox_net::Client,
    std::path::PathBuf,
) {
    let host_repo = scratch("host");
    init(&host_repo);
    let cfg = HostConfig {
        repo: Some(host_repo.clone()),
        min_isolation: playbox_runner::Isolation::None,
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg)
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let client = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let local = scratch("local");
    let (_, bundle) = client.pull("main").await.unwrap();
    let bundle_file = scratch("bundle").join("b.bundle");
    std::fs::write(&bundle_file, bundle).unwrap();
    std::fs::remove_dir_all(&local).unwrap();
    let out = Command::new("git")
        .args(["clone", "-q", "-b", "main"])
        .arg(&bundle_file)
        .arg(&local)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(&local, &["config", "user.email", "m@e.com"]);
    git(&local, &["config", "user.name", "member"]);
    std::fs::write(local.join("feature.txt"), "feature\n").unwrap();
    git(&local, &["add", "-A"]);
    git(&local, &["commit", "-q", "-m", "add feature"]);
    (host, host_repo, client, local)
}

#[tokio::test]
async fn a_push_waits_for_review_and_accepting_fast_forwards_main() {
    let (_host, repo, client, local) = setup().await;
    let before = git(&repo, &["rev-parse", "main"]);
    let bundle = playbox_git::bundle_create(&local, "main").await.unwrap();
    let reference = client.push("main", &bundle).await.unwrap();

    assert_eq!(
        git(&repo, &["rev-parse", "main"]),
        before,
        "a push alone never moves main"
    );
    let list = review::pending(&repo).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].reference, reference);
    let shown = review::show(&repo, &reference, "main").await.unwrap();
    assert!(
        shown.contains("add feature") && shown.contains("feature.txt"),
        "{shown}"
    );

    let new_tip = review::accept(&repo, &reference, "main").await.unwrap();
    assert_eq!(git(&repo, &["rev-parse", "main"]), new_tip);
    assert_eq!(
        std::fs::read_to_string(repo.join("feature.txt")).unwrap(),
        "feature\n",
        "the checked-out work tree moved with main"
    );
    assert!(
        review::pending(&repo).await.unwrap().is_empty(),
        "an accepted push is no longer pending"
    );
    assert!(
        !git(&repo, &["for-each-ref", "refs/playbox/accepted/"]).is_empty(),
        "the decision is kept"
    );
}

#[tokio::test]
async fn rejecting_leaves_main_alone_and_keeps_the_record() {
    let (_host, repo, client, local) = setup().await;
    let before = git(&repo, &["rev-parse", "main"]);
    let reference = client
        .push(
            "main",
            &playbox_git::bundle_create(&local, "main").await.unwrap(),
        )
        .await
        .unwrap();
    review::reject(&repo, &reference).await.unwrap();
    assert_eq!(git(&repo, &["rev-parse", "main"]), before);
    assert!(review::pending(&repo).await.unwrap().is_empty());
    assert!(!git(&repo, &["for-each-ref", "refs/playbox/rejected/"]).is_empty());
    assert!(!repo.join("feature.txt").exists());
}

#[tokio::test]
async fn a_push_not_built_on_the_tip_is_not_merged() {
    let (_host, repo, client, local) = setup().await;
    let reference = client
        .push(
            "main",
            &playbox_git::bundle_create(&local, "main").await.unwrap(),
        )
        .await
        .unwrap();
    // The host moves on after the member branched.
    std::fs::write(repo.join("host.txt"), "host\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "host moves"]);
    let tip = git(&repo, &["rev-parse", "main"]);
    let e = review::accept(&repo, &reference, "main").await.unwrap_err();
    assert!(format!("{e:#}").contains("rebase"), "{e:#}");
    assert_eq!(git(&repo, &["rev-parse", "main"]), tip, "main is untouched");
    assert_eq!(
        review::pending(&repo).await.unwrap().len(),
        1,
        "still pending"
    );
}

#[tokio::test]
async fn only_pending_member_refs_can_be_accepted() {
    let (_host, repo, _client, _local) = setup().await;
    for bad in [
        "refs/heads/main",
        "refs/playbox/members/",
        "refs/playbox/members/x",
        "refs/tags/v1",
    ] {
        assert!(
            review::accept(&repo, bad, "main").await.is_err(),
            "{bad} must be refused"
        );
        assert!(
            review::reject(&repo, bad).await.is_err(),
            "{bad} must be refused"
        );
    }
}

#[tokio::test]
async fn an_uncommitted_change_on_the_host_blocks_accept_instead_of_being_overwritten() {
    let (_host, repo, client, local) = setup().await;
    let reference = client
        .push(
            "main",
            &playbox_git::bundle_create(&local, "main").await.unwrap(),
        )
        .await
        .unwrap();
    std::fs::write(repo.join("feature.txt"), "host's own unsaved work\n").unwrap();
    assert!(review::accept(&repo, &reference, "main").await.is_err());
    assert_eq!(
        std::fs::read_to_string(repo.join("feature.txt")).unwrap(),
        "host's own unsaved work\n"
    );
    assert_eq!(review::pending(&repo).await.unwrap().len(), 1);
}
