//! A plain HTTP client (no Playbox code) driving a room through the bridge.

use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use playbox_runner::{Limits, ProcessRunner};
use serde_json::{json, Value};
use std::{path::Path, process::Command, sync::Arc, time::Duration};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

async fn start(token: Option<&str>) -> (Host, String) {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("playbox-bridge-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@e.com"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "i"]);
    let cfg = HostConfig { min_isolation: playbox_runner::Isolation::None,
        repo: Some(dir),
        runner: Some(Arc::new(ProcessRunner)),
        limits: Limits { cpus: 2.0, mem_mib: 1024, timeout_secs: 60, network: false },
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let client = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let (listener, addr) = playbox_bridge::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{addr}");
    let t = token.map(String::from);
    let u = url.clone();
    tokio::spawn(async move { playbox_bridge::serve(client, listener, t, u).await });
    (host, url)
}

fn rpc(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

#[tokio::test]
async fn stock_http_client_uses_the_room() {
    let (_host, url) = start(None).await;
    let http = reqwest::Client::new();

    let card: Value = http.get(format!("{url}/.well-known/agent-card.json")).send().await.unwrap().json().await.unwrap();
    assert_eq!(card["supportedInterfaces"][0]["protocolBinding"], "JSONRPC");
    assert_eq!(card["supportedInterfaces"][0]["url"], format!("{url}/jsonrpc"));
    assert!(card["skills"].as_array().unwrap().iter().any(|s| s["id"] == "run"));

    // Plain A2A text message.
    let r: Value = http.post(format!("{url}/jsonrpc")).json(&rpc(json!("abc"), "SendMessage",
        json!({"message": {"messageId": "m", "role": "ROLE_USER", "parts": [{"text": "hi"}]}}))).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["id"], "abc", "caller's id is preserved");
    assert!(r["result"]["message"]["parts"][0]["text"].as_str().unwrap().contains("Playbox room"));

    // A tracked task, then GetTask / ListTasks over HTTP.
    let run = json!({"message": {"messageId": "m2", "role": "ROLE_USER", "parts": [{"data": {"playbox": {"op": "run", "base": "main",
        "spec": {"cmd": ["sh", "-c", "echo http > h.txt; echo ok"], "image": "x", "cpus": 1.0, "mem_mib": 256, "timeout_secs": 20, "network": false}}}}]}});
    let r: Value = http.post(format!("{url}/jsonrpc")).json(&rpc(json!(7), "SendMessage", run)).send().await.unwrap().json().await.unwrap();
    let task = &r["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED", "{task}");
    let id = task["id"].as_str().unwrap();
    let r: Value = http.post(format!("{url}/jsonrpc")).json(&rpc(json!(8), "GetTask", json!({"id": id}))).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["result"]["id"], id);
    let r: Value = http.post(format!("{url}/jsonrpc")).json(&rpc(json!(9), "ListTasks", json!({}))).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["result"]["tasks"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn errors_keep_their_codes_and_join_is_not_reachable() {
    let (_host, url) = start(None).await;
    let http = reqwest::Client::new();
    let call = |m: &str, p: Value| {
        let (http, url, m) = (http.clone(), url.clone(), m.to_string());
        async move { http.post(format!("{url}/jsonrpc")).json(&rpc(json!(1), &m, p)).send().await.unwrap().json::<Value>().await.unwrap() }
    };
    assert_eq!(call("GetTask", json!({"id": "nope"})).await["error"]["code"], -32001);
    assert_eq!(call("Nope", json!({})).await["error"]["code"], -32601);
    assert_eq!(call("playbox.Join", json!({"invite": "x"})).await["error"]["code"], -32601);
    assert_eq!(call("SendStreamingMessage", json!({})).await["error"]["code"], -32004);
    let bad: Value = http.post(format!("{url}/jsonrpc")).body("{not json").send().await.unwrap().json().await.unwrap();
    assert_eq!(bad["error"]["code"], -32700);
}

#[tokio::test]
async fn bearer_token_is_enforced_and_open_listeners_are_refused() {
    let (_host, url) = start(Some("s3cret")).await;
    let http = reqwest::Client::new();
    let body = rpc(json!(1), "ListTasks", json!({}));
    let r = http.post(format!("{url}/jsonrpc")).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = http.post(format!("{url}/jsonrpc")).bearer_auth("wrong").json(&body).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r: Value = http.post(format!("{url}/jsonrpc")).bearer_auth("s3cret").json(&body).send().await.unwrap().json().await.unwrap();
    assert!(r["result"].is_object());
    let card: Value = http.get(format!("{url}/.well-known/agent-card.json")).send().await.unwrap().json().await.unwrap();
    assert!(card["securitySchemes"].is_object());

    // Non-loopback without a token must not start.
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let client = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let (l, a) = playbox_bridge::bind("0.0.0.0:0").await.unwrap();
    let err = playbox_bridge::serve(client, l, None, format!("http://{a}")).await.unwrap_err();
    assert!(format!("{err:#}").contains("without a bearer token"));
}
