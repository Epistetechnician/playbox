//! Tasks over the network run against a versioned workspace and commit per task.

use playbox_core::workspace::{Repo, Store};
use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use playbox_runner::{Limits, ProcessRunner, TaskSpec};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

fn sh(script: &str) -> TaskSpec {
    TaskSpec { timeout_secs: 20, ..TaskSpec::new(&["sh", "-c", script]) }
}

#[tokio::test]
async fn member_tasks_commit_to_their_own_branch() {
    let mut repo = Repo::new(Store::memory());
    repo.init("main", &[("a.txt", b"a")], "host", 1).unwrap();
    let repo = Arc::new(Mutex::new(repo));
    let cfg = HostConfig {
        runner: Some(Arc::new(ProcessRunner)),
        limits: Limits { cpus: 2.0, mem_mib: 1024, timeout_secs: 60, network: false },
        workspace: Some(repo.clone()),
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 5).unwrap().encode();
    let alice = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let bob = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();

    let (_, out, _) = alice.run_task("main", sh("echo from-alice > alice.txt")).await.unwrap();
    assert_eq!(out.exit_code, Some(0));
    alice.run_task("main", sh("cat alice.txt > copy.txt")).await.unwrap(); // sees its own earlier commit
    bob.run_task("main", sh("echo from-bob > bob.txt")).await.unwrap();

    let r = repo.lock().await;
    let a = r.head(&format!("members/{}", &alice.id().to_string()[..16])).unwrap();
    let b = r.head(&format!("members/{}", &bob.id().to_string()[..16])).unwrap();
    assert_eq!(r.read(&a, "copy.txt").unwrap().unwrap(), b"from-alice\n");
    assert_eq!(r.read(&a, "bob.txt").unwrap(), None, "branches are isolated");
    assert_eq!(r.read(&b, "bob.txt").unwrap().unwrap(), b"from-bob\n");
    assert_eq!(r.read(&r.head("main").unwrap(), "alice.txt").unwrap(), None, "main is untouched");
    let log = r.log(&format!("members/{}", &alice.id().to_string()[..16])).unwrap();
    assert_eq!(log.len(), 3); // init + two tasks
    assert!(log[0].1.tool_call.starts_with("rpc-"));
    drop(r);
    host.shutdown().await.unwrap();
}
