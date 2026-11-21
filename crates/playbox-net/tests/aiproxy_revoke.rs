//! Drill 4: Live `Host::revoke_member` -> immediate `BOX-7` teardown + `playbox-aiproxy` token rejection
//! + `T3` Confidential Space manifest pinned binary SHA-256 verification.

use iroh::SecretKey;
use playbox_net::{join_link, Host};
use playbox_runner::egress::{EgressConfig, EgressUpstream, Proxy};
use playbox_runner::secrets::SecretStore;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::net::{Ipv4Addr, TcpListener as StdTcpListener};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn free_port() -> u16 {
    let l = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    l.local_addr().unwrap().port()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

async fn http_call(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> std::io::Result<(u16, String)> {
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let resp_body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Ok((status, resp_body))
}

// REQ: GRANT-3, PROXY-7
#[tokio::test]
async fn drill4_revoke_member_kills_box7_and_aiproxy_token_and_t3_manifest_pins_binary_sha256() {
    let root = repo_root();
    let aiproxy_bin = root.join("target/playbox-aiproxy");
    if !aiproxy_bin.is_file() {
        eprintln!("skipping drill4: {} not built", aiproxy_bin.display());
        return;
    }

    // 1. Verify T3 Confidential Space manifest pins exact SHA-256 of `target/playbox-aiproxy`.
    let manifest_path = root.join("deploy/t3-confidential-space.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let pinned = manifest["components"]["playbox_aiproxy"]["reproducible_binary_sha256"]
        .as_str()
        .expect("reproducible_binary_sha256 in T3 manifest");
    let shasum_out = Command::new("shasum")
        .args(["-a", "256", aiproxy_bin.to_str().unwrap()])
        .output()
        .expect("shasum");
    let actual_hex = String::from_utf8_lossy(&shasum_out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert_eq!(
        pinned,
        format!("sha256:{actual_hex}"),
        "T3 manifest binary pin mismatch"
    );
    eprintln!("Drill 4A (T3 manifest pin verified): {pinned}");

    // 2. Spawn fake healthy upstream OpenAI server.
    let up_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let up_port = up_listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = up_listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf)).await;
                let body = r#"{"id":"chatcmpl-d4","object":"chat.completion","created":1795130000,"model":"gpt-drill4","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });

    // 3. Boot real hardened `target/playbox-aiproxy`.
    let tmp = std::env::temp_dir().join(format!("pbx-drill4-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let ap_port = free_port();
    let admin_key = "drill4-admin-key-9876543210";

    let child = Command::new(&aiproxy_bin)
        .args([
            "--listen",
            &format!("127.0.0.1:{ap_port}"),
            "--pprof-port",
            "0",
        ])
        .current_dir(&tmp)
        .env("ADMIN_KEY", admin_key)
        .env("LISTEN", format!("127.0.0.1:{ap_port}"))
        .env("SQL_DSN", "")
        .env("LOG_SQL_DSN", "")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn target/playbox-aiproxy");
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = KillOnDrop(child);

    let mut ready = false;
    for _ in 0..80 {
        if let Ok((200, _)) = http_call(
            ap_port,
            "GET",
            "/api/playbox/posture",
            &[("Authorization", admin_key)],
            "",
        )
        .await
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    assert!(ready, "playbox-aiproxy did not become ready");

    // Register model, channel, group `room:drill4`, and member token.
    let (st, b) = http_call(
        ap_port,
        "POST",
        "/api/model_config/gpt-drill4",
        &[("Authorization", admin_key), ("Content-Type", "application/json")],
        r#"{"model":"gpt-drill4","type":1,"owner":"openai","price":{"input_price":1,"input_price_unit":1000,"output_price":2,"output_price_unit":1000}}"#,
    )
    .await
    .unwrap();
    assert_eq!(st, 200, "model_config: {b}");

    let ch_body = format!(
        r#"{{"name":"d4-up","type":1,"key":"sk-d4","base_url":"http://127.0.0.1:{up_port}","models":["gpt-drill4"],"priority":10,"status":1}}"#
    );
    let (st, b) = http_call(
        ap_port,
        "POST",
        "/api/channel/",
        &[
            ("Authorization", admin_key),
            ("Content-Type", "application/json"),
        ],
        &ch_body,
    )
    .await
    .unwrap();
    assert_eq!(st, 200, "channel: {b}");

    let (st, b) = http_call(
        ap_port,
        "POST",
        "/api/group/room:drill4",
        &[
            ("Authorization", admin_key),
            ("Content-Type", "application/json"),
        ],
        r#"{"rpm_ratio":10,"tpm_ratio":10}"#,
    )
    .await
    .unwrap();
    assert_eq!(st, 200, "group: {b}");

    let (st, b) = http_call(
        ap_port,
        "POST",
        "/api/token/room:drill4",
        &[
            ("Authorization", admin_key),
            ("Content-Type", "application/json"),
        ],
        r#"{"name":"guest-member","quota":1.0,"subnets":["127.0.0.1/32","::1/128"]}"#,
    )
    .await
    .unwrap();
    assert_eq!(st, 200, "token: {b}");
    let tok_json: serde_json::Value = serde_json::from_str(&b).unwrap();
    let tok_id = tok_json["data"]["id"].as_i64().unwrap();
    let tok_key = tok_json["data"]["key"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 4. Start real `BOX-7` proxy for the guest member + real `playbox-net` Host + joined Guest.
    let sec_dir = tmp.join("secrets");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&sec_dir)
            .unwrap();
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(sec_dir.join("guest_tok"))
            .unwrap();
        f.write_all(tok_key.as_bytes()).unwrap();
    }
    let cfg = EgressConfig {
        upstreams: vec![EgressUpstream::aiproxy_preset(
            "llm",
            format!("http://127.0.0.1:{ap_port}"),
            "guest_tok",
        )],
        log_file: None,
        max_log_bytes: None,
    };
    let box7 = Proxy::start_with_host_and_headers(
        &cfg,
        &SecretStore::new(sec_dir),
        &["llm".to_string()],
        Ipv4Addr::LOCALHOST,
        "127.0.0.1",
        &BTreeMap::new(),
    )
    .await
    .unwrap();
    let box7_port = box7.port();
    let box7_path = format!(
        "{}/v1/chat/completions",
        box7.base_url("llm")
            .strip_prefix(&format!("http://127.0.0.1:{box7_port}"))
            .unwrap()
    );
    let box7_slot: Arc<Mutex<Option<Proxy>>> = Arc::new(Mutex::new(Some(box7)));

    let host = Host::start(SecretKey::from_bytes(&[41u8; 32]), true)
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 5)
        .unwrap()
        .encode();
    let guest_client = join_link(&link, SecretKey::from_bytes(&[42u8; 32]), true)
        .await
        .unwrap();
    let guest_id = guest_client.id();

    // Wire `Host::on_revoke` -> synchronously drop member's BOX-7 proxy + disable aiproxy token!
    let slot_for_hook = Arc::clone(&box7_slot);
    let revoked_notify = Arc::new(tokio::sync::Notify::new());
    let notify_bg = Arc::clone(&revoked_notify);
    host.on_revoke(move |revoked_id| {
        if *revoked_id == guest_id {
            // Immediately drop BOX-7 proxy so the guest's loopback port closes at once.
            let _ = slot_for_hook.lock().unwrap().take();
            let n = Arc::clone(&notify_bg);
            tokio::spawn(async move {
                let _ = http_call(
                    ap_port,
                    "POST",
                    &format!("/api/token/room:drill4/{tok_id}/status"),
                    &[
                        ("Authorization", admin_key),
                        ("Content-Type", "application/json"),
                    ],
                    r#"{"status":2}"#,
                )
                .await;
                n.notify_one();
            });
        }
    });

    // 5. Before revocation: request through BOX-7 -> target/playbox-aiproxy succeeds (HTTP 200).
    let chat_req = r#"{"model":"gpt-drill4","messages":[{"role":"user","content":"hi"}]}"#;
    let (st_before, body_before) = http_call(
        box7_port,
        "POST",
        &box7_path,
        &[("Content-Type", "application/json")],
        chat_req,
    )
    .await
    .unwrap();
    assert_eq!(st_before, 200, "pre-revoke call failed: {body_before}");
    eprintln!("Drill 4B (pre-revoke BOX-7 -> playbox-aiproxy): HTTP 200");

    // 6. Trigger `Host::revoke_member(&guest_id)` mid-session!
    assert!(host.revoke_member(&guest_id));
    tokio::time::timeout(Duration::from_secs(3), revoked_notify.notified())
        .await
        .expect("aiproxy token revoke hook completed");
    // Give the dropped TcpListener + aiproxy token cache a tiny tick.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 7. After revocation: BOTH BOX-7 and direct playbox-aiproxy reject the member immediately!
    let box7_after = http_call(
        box7_port,
        "POST",
        &box7_path,
        &[("Content-Type", "application/json")],
        chat_req,
    )
    .await;
    assert!(
        box7_after.is_err() || box7_after.as_ref().is_ok_and(|(c, _)| *c >= 400),
        "BOX-7 must reject/close after Host::revoke_member, got: {box7_after:?}"
    );

    let bearer = format!("Bearer {tok_key}");
    let (ap_st_after, ap_body_after) = http_call(
        ap_port,
        "POST",
        "/v1/chat/completions",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
        ],
        chat_req,
    )
    .await
    .unwrap();
    assert!(
        ap_st_after == 401 || ap_st_after == 403,
        "playbox-aiproxy must reject revoked token (401/403), got {ap_st_after}: {ap_body_after}"
    );
    eprintln!(
        "Drill 4C (post-revoke): BOX-7 closed ({}), playbox-aiproxy rejected HTTP {}",
        box7_after.is_err(),
        ap_st_after
    );

    guest_client.close().await;
    let _ = host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp);
}
