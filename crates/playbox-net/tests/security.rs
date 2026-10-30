//! Attacks on the host from inside a task. Each test plays a malicious member whose task can write
//! anything into its working directory, and checks that nothing runs on, leaks from, or exhausts the
//! host. The unsafe in-process runner stands in for "arbitrary code in the sandbox": it can do
//! exactly what a task that escaped nothing but controls its own working directory can do.

use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use playbox_runner::{Isolation, ProcessRunner, TaskSpec};
use std::{path::Path, process::Command, sync::Arc, time::Duration};

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

static SCRATCH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn scratch(name: &str) -> std::path::PathBuf {
    let n = SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("playbox-sec-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn repo(name: &str) -> std::path::PathBuf {
    let dir = scratch(name);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@e.com"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "i"]);
    dir
}

async fn member(repo: std::path::PathBuf) -> (Host, playbox_net::Client) {
    let cfg = HostConfig {
        repo: Some(repo),
        runner: Some(Arc::new(ProcessRunner)),
        min_isolation: Isolation::None,
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
    (host, client)
}

fn task(script: &str) -> TaskSpec {
    TaskSpec {
        timeout_secs: 30,
        ..TaskSpec::new(&["sh", "-c", script])
    }
}

/// A task replaces its worktree's `.git` pointer with a `.git` directory whose config defines a
/// clean filter, and marks every file as using it. If the host then runs `git add` against that
/// directory, git runs the filter command *on the host*, outside the sandbox.
#[tokio::test]
async fn a_task_cannot_make_the_host_run_commands_through_a_forged_dot_git() {
    let marker = scratch("marker").join("PWNED");
    let (host, client) = member(repo("gitfilter")).await;
    let script = format!(
        r#"rm -f .git && mkdir -p .git/objects .git/refs &&
printf 'ref: refs/heads/main\n' > .git/HEAD &&
printf '[core]\n\trepositoryformatversion = 0\n[filter "evil"]\n\tclean = touch {m}; cat\n\tsmudge = cat\n' > .git/config &&
printf '* filter=evil\n' > .gitattributes && echo payload > new.txt"#,
        m = marker.display()
    );
    // Whether the host answers with a patch or an error does not matter; what matters is the marker.
    let _ = client.run_task("main", task(&script)).await;
    assert!(
        !marker.exists(),
        "the host ran a command chosen by the task (sandbox escape through git)"
    );
    drop(host);
}

/// The same attack through a `.git` *file* that points at an attacker-made git directory.
#[tokio::test]
async fn a_task_cannot_redirect_git_with_a_gitdir_file() {
    let marker = scratch("marker2").join("PWNED");
    let (host, client) = member(repo("gitfile")).await;
    let script = format!(
        r#"mkdir -p evil/objects evil/refs && printf 'ref: refs/heads/main\n' > evil/HEAD &&
printf '[core]\n\trepositoryformatversion = 0\n[filter "evil"]\n\tclean = touch {m}; cat\n' > evil/config &&
printf 'gitdir: %s/evil\n' "$PWD" > .git && printf '* filter=evil\n' > .gitattributes && echo payload > new.txt"#,
        m = marker.display()
    );
    let _ = client.run_task("main", task(&script)).await;
    assert!(
        !marker.exists(),
        "the host ran a command chosen by the task via a gitdir redirect"
    );
    drop(host);
}

/// A task that leaves a huge sparse file must not make the host read it, hash it, or send it back:
/// it is refused up front and quickly, and the host carries on.
#[tokio::test]
async fn a_huge_output_file_is_refused_quickly_and_the_host_survives() {
    let (host, client) = member(repo("huge")).await;
    // 8 GiB sparse file: instant to create, enormous to read.
    let started = std::time::Instant::now();
    let r = client
        .run_task(
            "main",
            task("dd if=/dev/null of=big.bin bs=1 count=0 seek=8589934592 2>/dev/null; echo done"),
        )
        .await;
    let e = format!("{:#}", r.expect_err("an 8 GiB result must be refused"));
    assert!(e.contains("exceeds"), "{e}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "refusal took {:?}",
        started.elapsed()
    );
    assert_eq!(client.echo("alive").await.unwrap(), "alive");
    drop(host);
}

/// Many tiny files are the other way to exhaust a host: refused by count.
#[tokio::test]
async fn an_enormous_number_of_files_is_refused() {
    let (host, client) = member(repo("manyfiles")).await;
    let r = client.run_task("main", task("mkdir d && cd d && i=0; while [ $i -lt 2000 ]; do : > f$i; i=$((i+1)); done; echo ok")).await;
    // 2000 files is fine and must still work (the limit is far higher); this guards against over-blocking.
    assert!(r.is_ok(), "{:?}", r.err());
    drop(host);
}

/// A repo that legitimately defines a filter (git-lfs does) must not let a task choose when it runs:
/// the task controls `.gitattributes` in its checkout.
#[tokio::test]
async fn a_task_cannot_trigger_filters_the_repo_defines_through_gitattributes() {
    let marker = scratch("marker3").join("RAN");
    let r = repo("repofilter");
    git(
        &r,
        &[
            "config",
            "filter.corp.clean",
            &format!("touch {}; cat", marker.display()),
        ],
    );
    git(&r, &["config", "filter.corp.smudge", "cat"]);
    let (host, client) = member(r).await;
    let _ = client
        .run_task(
            "main",
            task("printf '* filter=corp\\n' > .gitattributes; echo payload > new.txt"),
        )
        .await;
    assert!(
        !marker.exists(),
        "the task's .gitattributes selected a filter command on the host"
    );
    drop(host);
}

/// An embedded repository inside the checkout, with a hostile config, must not be run either.
#[tokio::test]
async fn a_nested_repository_with_hostile_config_is_inert() {
    let marker = scratch("marker4").join("RAN");
    let (host, client) = member(repo("nested")).await;
    let script = format!(
        r#"mkdir -p sub/.git/objects sub/.git/refs && printf 'ref: refs/heads/main\n' > sub/.git/HEAD &&
printf '[core]\n\tfsmonitor = touch {m}\n\trepositoryformatversion = 0\n[filter "evil"]\n\tclean = touch {m}; cat\n' > sub/.git/config &&
printf '* filter=evil\n' > sub/.gitattributes && echo x > sub/f.txt"#,
        m = marker.display()
    );
    let r = client.run_task("main", task(&script)).await;
    assert!(
        !marker.exists(),
        "a nested repository's config ran on the host"
    );
    // The host stays usable and the legitimate part of the result still comes back.
    assert_eq!(client.echo("alive").await.unwrap(), "alive");
    let _ = r;
    drop(host);
}

// ---- the unauthenticated surface: what a stranger with a valid iroh identity can do -------------

use a2a::JsonRpcRequest;
use iroh::{endpoint::presets, Endpoint};
use playbox_net::wire::Frame;

async fn stranger() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(new_secret_key().unwrap())
        .bind()
        .await
        .unwrap()
}

fn frame(method: &str) -> Vec<u8> {
    serde_json::to_vec(&Frame::new(
        JsonRpcRequest::new(1.into(), method, None),
        None,
    ))
    .unwrap()
}

async fn host_alone() -> Host {
    Host::start_with(
        new_secret_key().unwrap(),
        true,
        HostConfig {
            idle_timeout: Duration::from_secs(2),
            max_connections: 3,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

/// Asking the host a question: did it answer with JSON-RPC at all?
async fn ask(conn: &iroh::endpoint::Connection, body: &[u8]) -> Result<Vec<u8>, String> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    send.write_all(body).await.map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    recv.read_to_end(1 << 20).await.map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_stranger_cannot_make_the_host_swallow_a_huge_request() {
    let host = host_alone().await;
    let ep = stranger().await;
    let conn = ep.connect(host.addr(), playbox_net::ALPN).await.unwrap();
    // 8 MiB of junk where a real request would be a few hundred bytes. Before authenticating, the host
    // must stop reading at a small limit and hang up, not buffer it all and answer.
    let mut junk = vec![b' '; 8 << 20];
    junk[0] = b'{';
    let reply = ask(&conn, &junk).await;
    assert!(
        reply.is_err(),
        "the host read and answered an 8 MiB unauthenticated request: {:?}",
        reply.map(|r| r.len())
    );
    // It still serves the next stranger.
    let ep2 = stranger().await; // keep the endpoint alive: dropping it closes its connections
    let conn2 = ep2.connect(host.addr(), playbox_net::ALPN).await.unwrap();
    let again = ask(&conn2, &frame("playbox.GetAgentCard")).await;
    assert!(
        again.is_ok(),
        "the host stopped serving after the oversized request: {:?}; close reason: {:?}",
        again.err(),
        conn2.close_reason()
    );
}

#[tokio::test]
async fn silent_connections_are_dropped() {
    let host = host_alone().await;
    let ep = stranger().await;
    let conn = ep.connect(host.addr(), playbox_net::ALPN).await.unwrap();
    // Say nothing. The host must close the connection on its own, not hold it open forever.
    let closed = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await;
    assert!(
        closed.is_ok(),
        "an idle unauthenticated connection was never closed"
    );
}

#[tokio::test]
async fn the_host_caps_simultaneous_connections() {
    let host = host_alone().await; // max_connections = 3
    let ep = stranger().await;
    let mut conns = Vec::new();
    for _ in 0..6 {
        conns.push(ep.connect(host.addr(), playbox_net::ALPN).await.unwrap());
    }
    let mut served = 0;
    for c in &conns {
        if ask(c, &frame("playbox.GetAgentCard")).await.is_ok() {
            served += 1;
        }
    }
    assert!(
        (1..=3).contains(&served),
        "{served} of 6 connections were served; the cap is 3"
    );
}

/// The cap must not break an honest client whose first request on a fresh connection is large, such as
/// a member reconnecting with the grant it already holds and pushing a big bundle at once.
#[tokio::test]
async fn a_large_first_request_on_a_new_connection_still_works_for_a_member() {
    let r = repo("bigfirst");
    let cfg = HostConfig {
        repo: Some(r),
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg)
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let first = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let second = first.sibling(host.addr()).await.unwrap(); // new connection, nothing sent on it yet

    let work = repo("bigfirst-client");
    let mut blob = Vec::with_capacity(3 << 20);
    let mut x: u64 = 0x9E3779B97F4A7C15;
    while blob.len() < (3 << 20) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        blob.extend_from_slice(&x.to_le_bytes());
    }
    std::fs::write(work.join("blob.bin"), &blob).unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "blob"]);
    let bundle = playbox_git::bundle_create(&work, "main").await.unwrap();
    assert!(bundle.len() > 2 << 20);
    let stored = second
        .push("main", &bundle)
        .await
        .expect("a member's large first request must be accepted");
    assert!(stored.starts_with("refs/playbox/members/"));
}

// ---- forging authority from the holder's side of a Biscuit grant --------------------------------
//
// A holder can append blocks to its own grant. Blocks may only restrict. These try to make a block
// *widen* the grant by supplying facts the authorizer is supposed to supply, or rights it never had.

use playbox_net::grants::attenuate;
use playbox_net::Client;
use playbox_runner::Limits;

async fn bare_member() -> (Host, Client) {
    let cfg = HostConfig {
        repo: Some(repo("forge")),
        runner: Some(Arc::new(ProcessRunner)),
        min_isolation: Isolation::None,
        limits: Limits {
            cpus: 1.0,
            mem_mib: 256,
            timeout_secs: 30,
            network: false,
            allowed_hosts: vec![],
        },
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg)
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 2)
        .unwrap()
        .encode();
    let client = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    (host, client)
}

fn refused(r: anyhow::Result<impl std::fmt::Debug>) -> bool {
    match r {
        Ok(v) => {
            eprintln!("unexpectedly allowed: {v:?}");
            false
        }
        Err(e) => {
            format!("{e:#}").contains("not authorized") || format!("{e:#}").contains("refused")
        }
    }
}

#[tokio::test]
async fn a_block_cannot_supply_the_facts_the_host_checks_requests_against() {
    let (_host, c) = bare_member().await;
    let g = c.grant().unwrap();
    // The host says req_net(1) for a networked task; the grant checks req_net($n), $n <= 0.
    // A block claiming req_net(0), req_cpu(0), req_mem(0), req_timeout(0) must not stand in for it.
    let forged = attenuate(
        &g,
        "req_net(0); req_cpu(0); req_mem(0); req_timeout(0); req_cmd(\"true\"); now(0);",
    )
    .unwrap();
    c.set_grant(forged);
    let over = TaskSpec {
        network: true,
        cpus: 4.0,
        mem_mib: 99_999,
        timeout_secs: 3000,
        ..TaskSpec::new(&["true"])
    };
    assert!(
        refused(c.run_task("main", over).await),
        "forged req_* facts widened the grant"
    );
}

#[tokio::test]
async fn a_block_cannot_grant_rights_the_holder_does_not_have() {
    let (_host, c) = bare_member().await;
    // The member narrows itself to echo only, then tries to talk its way back to `run` with a fact and a rule. (A policy such as `allow if true` cannot even be
    // written into a block: Biscuit's parser refuses it.)
    c.attenuate(r#"check if operation("echo");"#).unwrap();
    let forged = attenuate(
        &c.grant().unwrap(),
        r#"right("run"); right($op) <- operation($op);"#,
    )
    .unwrap();
    c.set_grant(forged);
    assert!(
        refused(c.run_task("main", TaskSpec::new(&["true"])).await),
        "a later block undid an earlier restriction"
    );
    assert_eq!(c.echo("still allowed").await.unwrap(), "still allowed");
}

#[tokio::test]
async fn a_block_cannot_make_an_expired_grant_current() {
    let cfg = HostConfig {
        grant_ttl: Some(Duration::ZERO),
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg)
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let c = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    assert!(
        refused(c.echo("late").await),
        "precondition: the grant is already expired"
    );
    let forged = attenuate(&c.grant().unwrap(), "now(0); check if true;").unwrap();
    c.set_grant(forged);
    assert!(
        refused(c.echo("late").await),
        "a block's now(0) revived an expired grant"
    );
}

#[tokio::test]
async fn a_block_cannot_move_a_grant_to_another_identity() {
    let (host, alice) = bare_member().await;
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let mallory = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    // Mallory takes Alice's grant and appends the fact the host would supply for Alice.
    let forged = attenuate(
        &alice.grant().unwrap(),
        &format!("subject(\"{}\");", alice.id()),
    )
    .unwrap();
    mallory.set_grant(forged);
    assert!(
        refused(mallory.echo("i am alice").await),
        "a stolen grant worked after appending subject(...)"
    );
}

#[tokio::test]
async fn garbage_and_oversized_grants_are_refused_without_harm() {
    let (host, c) = bare_member().await;
    for g in ["", "not base64 !!", "AAAA", &"A".repeat(40_000)] {
        let stranger = Client::connect(host.addr(), new_secret_key().unwrap(), true)
            .await
            .unwrap();
        stranger.set_grant(g.to_string());
        assert!(
            refused(stranger.echo("x").await),
            "grant {:?} was accepted",
            &g[..g.len().min(12)]
        );
    }
    // And the host is fine afterwards.
    assert_eq!(c.echo("ok").await.unwrap(), "ok");
}

// ---- a member (or stranger) using more than a fair share ----------------------------------------

fn bundle_of_size(name: &str, bytes: usize) -> Vec<u8> {
    let work = repo(name);
    let mut blob = Vec::with_capacity(bytes);
    let mut x: u64 = 0x2545F4914F6CDD1D;
    while blob.len() < bytes {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        blob.extend_from_slice(&x.to_le_bytes());
    }
    std::fs::write(work.join("blob.bin"), &blob).unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "blob"]);
    futures_block(playbox_git::bundle_create(&work, "main")).unwrap()
}

fn futures_block<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_cannot_fill_the_hosts_repository_without_limit() {
    let cfg = HostConfig {
        repo: Some(repo("quota")),
        max_push_bytes: 1 << 20,
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg)
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 2)
        .unwrap()
        .encode();
    let alice = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let bob = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();

    let b = bundle_of_size("quota-b", 600 * 1024);
    assert!(
        alice.push("main", &b).await.is_ok(),
        "the first push is within the 1 MiB quota"
    );
    let e = format!("{:#}", alice.push("again", &b).await.unwrap_err());
    assert!(e.contains("quota"), "{e}");
    // Quotas are per member: Bob is unaffected by Alice using hers up.
    assert!(bob.push("main", &b).await.is_ok());
}

#[tokio::test]
async fn one_identity_cannot_take_every_connection_slot() {
    let host = Host::start_with(
        new_secret_key().unwrap(),
        true,
        HostConfig {
            max_connections_per_peer: 2,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let hog = stranger().await;
    let mut held = Vec::new();
    for _ in 0..5 {
        held.push(hog.connect(host.addr(), playbox_net::ALPN).await.unwrap());
    }
    let mut served = 0;
    for c in &held {
        if ask(c, &frame("playbox.GetAgentCard")).await.is_ok() {
            served += 1;
        }
    }
    assert!(
        (1..=2).contains(&served),
        "{served} of 5 connections from one identity were served; the cap is 2"
    );
    // A different identity is not affected by the hog.
    let other = stranger().await;
    let c = other.connect(host.addr(), playbox_net::ALPN).await.unwrap();
    assert!(ask(&c, &frame("playbox.GetAgentCard")).await.is_ok());
}

/// Special files left behind (a FIFO nobody writes to, a socket, a deep directory chain) must not hang
/// or crash the host while it collects the result.
#[tokio::test]
async fn special_files_and_deep_nesting_do_not_hang_the_host() {
    let (host, client) = member(repo("special")).await;
    let started = std::time::Instant::now();
    let script = "mkfifo pipe; d=x; i=0; while [ $i -lt 200 ]; do mkdir -p $d 2>/dev/null || break; cd $d; i=$((i+1)); done; echo ok > ../../../deep.txt 2>/dev/null; echo done";
    let r = tokio::time::timeout(
        Duration::from_secs(60),
        client.run_task("main", task(script)),
    )
    .await;
    assert!(
        r.is_ok(),
        "collecting the result hung for {:?}",
        started.elapsed()
    );
    assert_eq!(client.echo("alive").await.unwrap(), "alive");
    drop(host);
}

// ---- disk: a task cannot fill the host while it runs --------------------------------------------

use playbox_runner::Quota;

/// Whether Docker answers within a few seconds. A hung daemon would otherwise stall every test that wants it for a minute.
fn docker_answers() -> bool {
    use std::process::Stdio;
    let Ok(mut child) = Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < std::time::Duration::from_secs(8) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                return false;
            }
        }
    }
}

/// A task that writes ~10 MiB every 200 ms for 20 s: a runaway the result-size check alone would only
/// notice after the fact (or never, if the host's disk filled first).
const SLOW_WRITER: &str = "i=0; while [ $i -lt 100 ]; do dd if=/dev/zero bs=1048576 count=10 >> quota_probe.bin 2>/dev/null; sleep 0.2; i=$((i+1)); done; echo finished";

async fn quota_member(quota: Quota) -> (Host, playbox_net::Client) {
    let cfg = HostConfig {
        repo: Some(repo("quota")),
        runner: Some(Arc::new(ProcessRunner)),
        min_isolation: Isolation::None,
        scratch_quota: quota,
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
    (host, client)
}

fn leftover_probe_files() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    if let Ok(top) = std::fs::read_dir(std::env::temp_dir()) {
        for e in top
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("playbox-"))
        {
            if let Ok(inner) = std::fs::read_dir(e.path().join("ws")) {
                found.extend(
                    inner
                        .flatten()
                        .map(|f| f.path())
                        .filter(|p| p.ends_with("quota_probe.bin")),
                );
            }
        }
    }
    found
}

#[tokio::test]
async fn a_task_that_keeps_writing_is_stopped_at_the_quota_and_its_files_are_removed() {
    let (host, client) = quota_member(Quota {
        max_bytes: 30 << 20,
        max_entries: 1000,
    })
    .await;
    let started = std::time::Instant::now();
    let r = client
        .run_task(
            "main",
            TaskSpec {
                timeout_secs: 60,
                ..TaskSpec::new(&["sh", "-c", SLOW_WRITER])
            },
        )
        .await;
    let took = started.elapsed();
    // A stopped task is not a finished one: the member is refused, and told why.
    let e = format!(
        "{:#}",
        r.expect_err("a runaway writer must not come back as a finished task")
    );
    assert!(
        e.contains("scratch quota"),
        "the member was not told why: {e}"
    );
    assert!(
        took < Duration::from_secs(15),
        "stopping took {took:?}; the writer would have run for 20 s"
    );
    assert!(
        leftover_probe_files().is_empty(),
        "the oversized file was left on the host: {:?}",
        leftover_probe_files()
    );
    assert_eq!(client.echo("alive").await.unwrap(), "alive");
    drop(host);
}

#[tokio::test]
async fn a_task_within_the_quota_is_left_alone() {
    let (host, client) = quota_member(Quota {
        max_bytes: 30 << 20,
        max_entries: 1000,
    })
    .await;
    let (_, out, patch) = client
        .run_task("main", TaskSpec { timeout_secs: 30, ..TaskSpec::new(&["sh", "-c", "dd if=/dev/zero of=ok.bin bs=1048576 count=5 2>/dev/null; sleep 0.6; echo finished"]) })
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout, "finished\n");
    assert!(!patch.is_empty());
    drop(host);
}

#[tokio::test]
async fn a_flood_of_tiny_files_is_stopped_by_the_entry_limit() {
    let (host, client) = quota_member(Quota {
        max_bytes: 1 << 30,
        max_entries: 300,
    })
    .await;
    let script = "i=0; while [ $i -lt 5000 ]; do : > f$i; i=$((i+1)); if [ $((i % 100)) -eq 0 ]; then sleep 0.15; fi; done; echo finished";
    let started = std::time::Instant::now();
    let r = client
        .run_task(
            "main",
            TaskSpec {
                timeout_secs: 60,
                ..TaskSpec::new(&["sh", "-c", script])
            },
        )
        .await;
    let e = format!("{:#}", r.expect_err("a flood of files must be stopped"));
    assert!(e.contains("files and directories"), "{e}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    drop(host);
}

/// A Docker container must die with the stopped run, not keep writing to the host's disk.
#[tokio::test]
async fn a_stopped_docker_task_leaves_no_container_running() {
    if !docker_answers() {
        eprintln!("skipping: docker is not running");
        return;
    }
    let cfg = HostConfig {
        repo: Some(repo("dockerquota")),
        runner: Some(Arc::new(playbox_runner::DockerRunner::default())),
        scratch_quota: Quota {
            max_bytes: 20 << 20,
            max_entries: 1000,
        },
        ..Default::default()
    };
    let Ok(host) = Host::start_with(new_secret_key().unwrap(), true, cfg).await else {
        eprintln!("skipping: this machine's docker cannot enforce the limits grants promise");
        return;
    };
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let client = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let script = "i=0; while [ $i -lt 100 ]; do dd if=/dev/zero bs=1048576 count=10 >> /workspace/quota_probe.bin 2>/dev/null; sleep 0.2; i=$((i+1)); done; echo finished";
    eprintln!("[docker quota] host up, running the task");
    let started = std::time::Instant::now();
    let r = client
        .run_task(
            "main",
            TaskSpec {
                timeout_secs: 60,
                mem_mib: 128,
                ..TaskSpec::new(&["sh", "-c", script])
            },
        )
        .await;
    eprintln!(
        "[docker quota] task call returned after {:?}",
        started.elapsed()
    );
    let e = format!(
        "{:#}",
        r.expect_err("the writer must be stopped, not left to finish or time out")
    );
    assert!(e.contains("scratch quota"), "{e}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "took {:?}: the quota did not stop the container promptly",
        started.elapsed()
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let running = Command::new("docker")
        .args(["ps", "--filter", "name=playbox-", "--format", "{{.Names}}"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&running.stdout).trim().is_empty(),
        "a container is still running: {}",
        String::from_utf8_lossy(&running.stdout)
    );
}
