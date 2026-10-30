use axum::{extract::State, routing::post, Json, Router};
use ed25519_dalek::SigningKey;
use playbox_core::record::hex;
use playbox_mcp::Server;
use playbox_rap::{serve, Rap};
use serde_json::{json, Value};
use std::sync::Once;
use std::time::Duration;
use tokio::{net::TcpListener, sync::mpsc};

const TOKEN: &str = "test-token-0123456789";
const MEMBER: &str = "member-id-copied-from-a-guests-output";

fn approver() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

/// The server reads `PLAYBOX_APPROVERS` when it is built, so set it once before any server exists.
fn pin_approver() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var(
            "PLAYBOX_APPROVERS",
            hex(&approver().verifying_key().to_bytes()),
        );
        // Rooms are kept under the playbox home; never use the real one.
        let home = std::env::temp_dir().join(format!("playbox-rap-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("PLAYBOX_HOME", home);
    });
}

async fn start_approving(wait: Duration) -> (String, tempfile_dir::Dir) {
    pin_approver();
    let home = tempfile_dir::Dir::new();
    let mut server = Server::new(home.path()).unwrap();
    server.mark_untrusted(MEMBER, "task:guest-1");
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let rap = Rap::with_approver(server, TOKEN.into(), vec![], base.clone(), approver(), wait);
    tokio::spawn(serve(l, rap));
    (base, home)
}

async fn start() -> (String, tempfile_dir::Dir) {
    pin_approver();
    let home = tempfile_dir::Dir::new();
    let server = Server::new(home.path()).unwrap();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(serve(
        l,
        Rap::new(server, TOKEN.into(), vec![], base.clone()),
    ));
    (base, home)
}

/// A callback receiver; every posted body is forwarded on the channel.
async fn receiver() -> (String, mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = Router::new()
        .route(
            "/cb",
            post(
                |State(tx): State<mpsc::UnboundedSender<Value>>, Json(v): Json<Value>| async move {
                    tx.send(v).ok();
                },
            ),
        )
        .with_state(tx);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/cb", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await });
    (url, rx)
}

fn invocation(op: &str, callback: &str) -> Value {
    json!({"operation": op, "arguments": {}, "id": "call-1", "call_id": null,
           "callback_url": callback, "group_id": "thread-1"})
}

#[tokio::test]
async fn toolset_lists_playbox_tools_and_needs_auth() {
    let (base, _h) = start().await;
    let c = reqwest::Client::new();
    let url = format!("{base}/.well-known/rap-toolset");
    assert_eq!(c.get(&url).send().await.unwrap().status(), 401);
    let m: Value = c
        .get(&url)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(m["endpoint"], format!("{base}/invoke"));
    let names: Vec<_> = m["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"playbox_delegate") && names.contains(&"playbox_status"));
}

#[tokio::test]
async fn invoke_acks_then_posts_tool_result() {
    let (base, _h) = start().await;
    let (cb, mut rx) = receiver().await;
    let r = reqwest::Client::new()
        .post(format!("{base}/invoke"))
        .bearer_auth(TOKEN)
        .json(&invocation("playbox_status", &cb))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got["type"], "tool_result");
    assert_eq!(got["id"], "call-1");
    assert_eq!(got["group_id"], "thread-1");
    assert!(got["text"].as_str().is_some());
}

#[tokio::test]
async fn unknown_operation_comes_back_as_an_error_result() {
    let (base, _h) = start().await;
    let (cb, mut rx) = receiver().await;
    reqwest::Client::new()
        .post(format!("{base}/invoke"))
        .bearer_auth(TOKEN)
        .json(&invocation("nope", &cb))
        .send()
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(got["text"].as_str().unwrap().starts_with("error:"));
}

#[tokio::test]
async fn refuses_missing_token_and_foreign_callback_hosts() {
    let (base, _h) = start().await;
    let c = reqwest::Client::new();
    let url = format!("{base}/invoke");
    let ok = invocation("playbox_status", "http://127.0.0.1:1/cb");
    assert_eq!(c.post(&url).json(&ok).send().await.unwrap().status(), 401);
    let foreign = invocation("playbox_status", "http://169.254.169.254/latest");
    assert_eq!(
        c.post(&url)
            .bearer_auth(TOKEN)
            .json(&foreign)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let file = invocation("playbox_status", "file:///etc/passwd");
    assert_eq!(
        c.post(&url)
            .bearer_auth(TOKEN)
            .json(&file)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
}

fn unique_room() -> String {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    format!("rap-test-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn invoke_revoke(base: &str, cb: &str) {
    let r = reqwest::Client::new()
        .post(format!("{base}/invoke"))
        .bearer_auth(TOKEN)
        .json(
            &json!({"operation": "playbox_revoke", "arguments": {"member": MEMBER}, "id": "call-9",
                      "call_id": null, "callback_url": cb, "group_id": "thread-1"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

async fn answer(url: &str, selected: usize) -> u16 {
    reqwest::Client::new()
        .post(url)
        .bearer_auth(TOKEN)
        .json(&json!({"id": "call-9", "selected": selected}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn approving_a_user_choice_spends_a_capability_and_retries() {
    let (base, _h) = start_approving(Duration::from_secs(30)).await;
    let (cb, mut rx) = receiver().await;
    invoke_revoke(&base, &cb).await;
    let ask = next(&mut rx).await;
    assert_eq!(ask["type"], "user_choice");
    assert_eq!(ask["id"], "call-9");
    assert_eq!(ask["default"], 1);
    assert!(ask["prompt"]
        .as_str()
        .unwrap()
        .contains("playbox_revoke(member-id-copied-from-a-guests-output)"));
    let url = ask["response_url"].as_str().unwrap();
    assert_eq!(answer(url, 0).await, 200);
    // The capability was accepted; this sidecar then fails only because it hosts no room.
    let done = next(&mut rx).await;
    assert_eq!(done["type"], "tool_result");
    assert!(
        done["text"]
            .as_str()
            .unwrap()
            .contains("not hosting a room"),
        "{done}"
    );
    // One answer per question.
    assert_eq!(answer(url, 0).await, 404);
}

#[tokio::test]
async fn denying_or_ignoring_a_user_choice_refuses_the_call() {
    let (base, _h) = start_approving(Duration::from_secs(30)).await;
    let (cb, mut rx) = receiver().await;
    invoke_revoke(&base, &cb).await;
    let ask = next(&mut rx).await;
    assert_eq!(answer(ask["response_url"].as_str().unwrap(), 1).await, 200);
    assert_eq!(next(&mut rx).await["text"], "error: approval denied");

    let (base, _h) = start_approving(Duration::from_millis(200)).await;
    let (cb, mut rx) = receiver().await;
    invoke_revoke(&base, &cb).await;
    assert_eq!(next(&mut rx).await["type"], "user_choice");
    assert_eq!(next(&mut rx).await["text"], "error: approval timed out");
}

#[tokio::test]
async fn choice_answers_need_the_token_and_a_valid_index() {
    let (base, _h) = start_approving(Duration::from_secs(30)).await;
    let (cb, mut rx) = receiver().await;
    invoke_revoke(&base, &cb).await;
    let ask = next(&mut rx).await;
    let url = ask["response_url"].as_str().unwrap();
    let c = reqwest::Client::new();
    let body = json!({"id": "call-9", "selected": 0});
    assert_eq!(c.post(url).json(&body).send().await.unwrap().status(), 401);
    assert_eq!(answer(url, 5).await, 400);
}

async fn call(base: &str, cb: &str, id: &str, op: &str, args: Value) {
    let r = reqwest::Client::new()
        .post(format!("{base}/invoke"))
        .bearer_auth(TOKEN)
        .json(&json!({"operation": op, "arguments": args, "id": id, "call_id": null,
                      "callback_url": cb, "group_id": "thread-1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// The first message of `kind` for call `id`, skipping anything else.
async fn wait_for(rx: &mut mpsc::UnboundedReceiver<Value>, kind: &str, pred: impl Fn(&Value) -> bool) -> Value {
    loop {
        let m = next(rx).await;
        if m["type"] == kind && pred(&m) {
            return m;
        }
    }
}

fn git_repo(home: &tempfile_dir::Dir) -> std::path::PathBuf {
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "h@example.com"],
        &["config", "user.name", "h"],
    ] {
        assert!(std::process::Command::new("git").args(args).current_dir(&repo).status().unwrap().success());
    }
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    for args in [&["add", "-A"][..], &["commit", "-q", "-m", "init"]] {
        assert!(std::process::Command::new("git").args(args).current_dir(&repo).status().unwrap().success());
    }
    repo
}

#[tokio::test]
async fn subscription_streams_new_room_items_only_and_ends_on_cancel() {
    let (base, h) = start().await;
    let (cb, mut rx) = receiver().await;
    let repo = git_repo(&h);
    call(&base, &cb, "create", "playbox_create", json!({"repo": repo, "local": true, "name": unique_room()})).await;
    let made = wait_for(&mut rx, "tool_result", |m| m["id"] == "create").await;
    assert!(!made["text"].as_str().unwrap().starts_with("error:"), "{made}");
    call(&base, &cb, "before", "playbox_say", json!({"text": "old news"})).await;
    wait_for(&mut rx, "tool_result", |m| m["id"] == "before").await;

    call(&base, &cb, "sub", "playbox_subscribe", json!({"interval_ms": 500})).await;
    let ok = wait_for(&mut rx, "tool_result", |m| m["id"] == "sub").await;
    assert_eq!(ok["subscription"], true);

    call(&base, &cb, "after", "playbox_say", json!({"text": "hello room"})).await;
    let ev = wait_for(&mut rx, "subscription_event", |_| true).await;
    assert_eq!(ev["tool_call_id"], "sub");
    // Inline in the subscribing thread; a non-associative event would start a child thread per item.
    assert_eq!(ev["associative"], true);
    assert_eq!(ev["group_id"], "thread-1");
    let item: Value = serde_json::from_str(ev["text"].as_str().unwrap()).unwrap();
    assert_eq!(item["text"], "hello room", "the backlog must not be replayed: {item}");
    assert_eq!(item["untrusted"], true);

    let r = reqwest::Client::new()
        .post(format!("{base}/cancel_tool_call"))
        .bearer_auth(TOKEN)
        .json(&json!({"thread_id": "thread-1", "tool_call_id": "sub"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let end = wait_for(&mut rx, "subscription_event", |m| m["final"] == true).await;
    assert!(end["text"].as_str().unwrap().contains("cancelled"));
    call(&base, &cb, "later", "playbox_say", json!({"text": "nobody hears this"})).await;
    wait_for(&mut rx, "tool_result", |m| m["id"] == "later").await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    while let Ok(m) = rx.try_recv() {
        assert_ne!(m["type"], "subscription_event", "events after cancel: {m}");
    }
}

#[tokio::test]
async fn closing_a_thread_ends_its_subscriptions() {
    let (base, h) = start().await;
    let (cb, mut rx) = receiver().await;
    let repo = git_repo(&h);
    call(&base, &cb, "create", "playbox_create", json!({"repo": repo, "local": true, "name": unique_room()})).await;
    wait_for(&mut rx, "tool_result", |m| m["id"] == "create").await;
    call(&base, &cb, "sub", "playbox_subscribe", json!({"interval_ms": 500})).await;
    wait_for(&mut rx, "tool_result", |m| m["id"] == "sub").await;
    let c = reqwest::Client::new();
    let url = format!("{base}/close_thread");
    assert_eq!(c.post(&url).json(&json!({"thread_id": "thread-1"})).send().await.unwrap().status(), 401);
    assert_eq!(c.post(&url).bearer_auth(TOKEN).json(&json!({"thread_id": "thread-1"})).send().await.unwrap().status(), 200);
    let end = wait_for(&mut rx, "subscription_event", |m| m["final"] == true).await;
    assert!(end["text"].as_str().unwrap().contains("thread closed"));
}

mod tempfile_dir {
    use std::path::{Path, PathBuf};
    pub struct Dir(PathBuf);
    impl Dir {
        pub fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "playbox-rap-{}-{}",
                std::process::id(),
                blake_ish()
            ));
            std::fs::remove_dir_all(&p).ok();
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
    fn blake_ish() -> usize {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}
