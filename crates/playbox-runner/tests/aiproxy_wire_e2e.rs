//! Drill 1: Zero-cost full-wire fault-injection E2E test.
//!
//! Real Rust `BOX-7` (`egress.rs`) -> real compiled hardened `target/playbox-aiproxy`
//! -> two local fake OpenAI upstreams (Channel 1 = `429 Retry-After: 1200`, Channel 2 = live SSE)
//! -> real SQLite `playbox.llm_ledger.v1` -> hybrid `Ed25519+ML-DSA-65` receipt -> `playbox gate verify`.

use playbox_runner::egress::{EgressConfig, EgressUpstream, Proxy};
use playbox_runner::secrets::SecretStore;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::net::{Ipv4Addr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

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

fn find_playbox_bin(root: &Path) -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("PLAYBOX_BIN").map(PathBuf::from),
        Some(root.join("target/release/playbox")),
        Some(root.join("target/debug/playbox")),
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin/playbox")),
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo/bin/playbox")),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

async fn post_json(
    http: &reqwest::Client,
    url: &str,
    auth: Option<&str>,
    body: &serde_json::Value,
) -> (u16, String) {
    let mut rb = http
        .post(url)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(body).unwrap());
    if let Some(a) = auth {
        rb = rb.header("Authorization", a);
    }
    let resp = rb.send().await.unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    (status, text)
}

async fn get_json(http: &reqwest::Client, url: &str, auth: &str) -> (u16, serde_json::Value) {
    let resp = http
        .get(url)
        .header("Authorization", auth)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    let val = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    (status, val)
}

async fn read_req_raw(stream: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 2048];
    let mut header_end = None;
    let mut content_len = 0usize;
    loop {
        let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut tmp)).await
        else {
            break;
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if header_end.is_none() {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let head = String::from_utf8_lossy(&buf[..pos]);
                for line in head.lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
            }
        }
        if let Some(he) = header_end {
            if buf.len() >= he + content_len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

// REQ: BOX-7, PROXY-1, PROXY-2, PROXY-3, PROXY-4, PROXY-7
#[tokio::test]
async fn drill1_box7_to_hardened_aiproxy_failover_budget_cutoff_and_pq_receipt() {
    let root = repo_root();
    let aiproxy_bin = root.join("target/playbox-aiproxy");
    if !aiproxy_bin.is_file() {
        eprintln!(
            "skipping drill1: hardened binary not built at {}",
            aiproxy_bin.display()
        );
        return;
    }

    // 1. Spawn Channel 1: always returns 429 with Retry-After: 1200 (20-minute stall trap).
    let ch1_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let ch1_port = ch1_listener.local_addr().unwrap().port();
    let ch1_hits = Arc::new(AtomicUsize::new(0));
    let ch1_hits_bg = Arc::clone(&ch1_hits);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = ch1_listener.accept().await {
            let hits = Arc::clone(&ch1_hits_bg);
            tokio::spawn(async move {
                let _ = read_req_raw(&mut s).await;
                hits.fetch_add(1, Ordering::SeqCst);
                let body =
                    r#"{"error":{"message":"rate limit exceeded","type":"rate_limit_error"}}"#;
                let resp = format!(
                    "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 1200\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });

    // 2. Spawn Channel 2: healthy OpenAI-compatible SSE upstream (normal + RUNAWAY_LOOP stream).
    let ch2_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let ch2_port = ch2_listener.local_addr().unwrap().port();
    let ch2_hits = Arc::new(AtomicUsize::new(0));
    let ch2_hits_bg = Arc::clone(&ch2_hits);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = ch2_listener.accept().await {
            let hits = Arc::clone(&ch2_hits_bg);
            tokio::spawn(async move {
                let raw = read_req_raw(&mut s).await;
                hits.fetch_add(1, Ordering::SeqCst);
                let is_runaway = raw.contains("RUNAWAY_LOOP");
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
                let _ = s.write_all(head.as_bytes()).await;
                if is_runaway {
                    for i in 0..400 {
                        let chunk = format!(
                            "data: {{\"id\":\"chatcmpl-run\",\"object\":\"chat.completion.chunk\",\"created\":1795130000,\"model\":\"gpt-drill\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"tok{i:04} \"}},\"finish_reason\":null}}]}}\n\n"
                        );
                        if s.write_all(chunk.as_bytes()).await.is_err() {
                            break;
                        }
                        let _ = s.flush().await;
                    }
                    let done = "data: {\"id\":\"chatcmpl-run\",\"object\":\"chat.completion.chunk\",\"created\":1795130000,\"model\":\"gpt-drill\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":400,\"total_tokens\":410}}\n\ndata: [DONE]\n\n";
                    let _ = s.write_all(done.as_bytes()).await;
                } else {
                    let c1 = "data: {\"id\":\"chatcmpl-ok\",\"object\":\"chat.completion.chunk\",\"created\":1795130000,\"model\":\"gpt-drill\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello \"},\"finish_reason\":null}]}\n\n";
                    let c2 = "data: {\"id\":\"chatcmpl-ok\",\"object\":\"chat.completion.chunk\",\"created\":1795130000,\"model\":\"gpt-drill\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"from ch2\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":45,\"total_tokens\":165}}\n\ndata: [DONE]\n\n";
                    let _ = s.write_all(c1.as_bytes()).await;
                    let _ = s.flush().await;
                    let _ = s.write_all(c2.as_bytes()).await;
                }
                let _ = s.shutdown().await;
            });
        }
    });

    // 3. Boot real hardened `target/playbox-aiproxy` on loopback with isolated temp SQLite.
    let tmp = std::env::temp_dir().join(format!("pbx-drill1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let ap_port = free_port();
    let admin_key = "drill1-admin-key-0123456789";
    let ap_base = format!("http://127.0.0.1:{ap_port}");

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
        .env("RETRY_TIMES", "3")
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

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Wait for posture endpoint to report ready & hardened.
    let mut ready = false;
    for _ in 0..80 {
        if let Ok(r) = http
            .get(format!("{ap_base}/api/playbox/posture"))
            .header("Authorization", admin_key)
            .send()
            .await
        {
            if r.status().is_success() {
                let v: serde_json::Value = serde_json::from_str(&r.text().await.unwrap()).unwrap();
                assert_eq!(v["data"]["playbox_hardened"], true);
                assert_eq!(v["data"]["mcp_disabled"], true);
                assert_eq!(v["data"]["web_search_disabled"], true);
                assert_eq!(v["data"]["loopback_only"], true);
                ready = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    assert!(ready, "playbox-aiproxy did not become ready in time");

    // 4. Register model config (`gpt-drill`), Channel 1 (429, high priority), Channel 2 (healthy SSE),
    //    Group `bench:drill1`, and two tokens (normal + $0.001 runaway cap).
    let (st, txt) = post_json(
        &http,
        &format!("{ap_base}/api/model_config/gpt-drill"),
        Some(admin_key),
        &serde_json::json!({
            "model": "gpt-drill",
            "type": 1,
            "owner": "openai",
            "price": {
                "input_price": 1.0,
                "input_price_unit": 1000,
                "output_price": 10.0,
                "output_price_unit": 1000
            }
        }),
    )
    .await;
    assert_eq!(st, 200, "save model_config failed: {txt}");

    let (st, txt) = post_json(
        &http,
        &format!("{ap_base}/api/channel/"),
        Some(admin_key),
        &serde_json::json!({
            "name": "drill-ch1-429",
            "type": 1,
            "key": "sk-fake-ch1",
            "base_url": format!("http://127.0.0.1:{ch1_port}"),
            "models": ["gpt-drill"],
            "priority": 100,
            "status": 1
        }),
    )
    .await;
    assert_eq!(st, 200, "add ch1 failed: {txt}");

    let (st, txt) = post_json(
        &http,
        &format!("{ap_base}/api/channel/"),
        Some(admin_key),
        &serde_json::json!({
            "name": "drill-ch2-sse",
            "type": 1,
            "key": "sk-fake-ch2",
            "base_url": format!("http://127.0.0.1:{ch2_port}"),
            "models": ["gpt-drill"],
            "priority": 1,
            "status": 1
        }),
    )
    .await;
    assert_eq!(st, 200, "add ch2 failed: {txt}");

    let (st, txt) = post_json(
        &http,
        &format!("{ap_base}/api/group/bench:drill1"),
        Some(admin_key),
        &serde_json::json!({"rpm_ratio": 10.0, "tpm_ratio": 10.0}),
    )
    .await;
    assert_eq!(st, 200, "create group failed: {txt}");

    let (st, txt) = post_json(
        &http,
        &format!("{ap_base}/api/token/bench:drill1"),
        Some(admin_key),
        &serde_json::json!({
            "name": "w0-normal",
            "quota": 5.0,
            "subnets": ["127.0.0.1/32", "::1/128"]
        }),
    )
    .await;
    assert_eq!(st, 200, "mint tok_a failed: {txt}");
    let tok_a_json: serde_json::Value = serde_json::from_str(&txt).unwrap();
    let tok_a_key = tok_a_json["data"]["key"].as_str().unwrap().to_string();

    let (st, txt) = post_json(
        &http,
        &format!("{ap_base}/api/token/bench:drill1"),
        Some(admin_key),
        &serde_json::json!({
            "name": "w1-tinycap",
            "quota": 0.001,
            "subnets": ["127.0.0.1/32", "::1/128"]
        }),
    )
    .await;
    assert_eq!(st, 200, "mint tok_b failed: {txt}");
    let tok_b_json: serde_json::Value = serde_json::from_str(&txt).unwrap();
    let tok_b_key = tok_b_json["data"]["key"].as_str().unwrap().to_string();

    tokio::time::sleep(Duration::from_millis(250)).await;

    // 5. Start real Rust BOX-7 egress proxy in front of `target/playbox-aiproxy`.
    let sec_dir = tmp.join("secrets");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&sec_dir)
            .unwrap();
        for (name, val) in [("tok_a", &tok_a_key), ("tok_b", &tok_b_key)] {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(sec_dir.join(name))
                .unwrap();
            f.write_all(val.as_bytes()).unwrap();
        }
    }
    let secrets = SecretStore::new(sec_dir.clone());
    let cfg = EgressConfig {
        upstreams: vec![
            EgressUpstream::aiproxy_preset("llm", &ap_base, "tok_a"),
            EgressUpstream::aiproxy_preset("llm_tiny", &ap_base, "tok_b"),
        ],
        log_file: Some(tmp.join("box7-egress.jsonl")),
        max_log_bytes: None,
    };
    let mut corr = BTreeMap::new();
    corr.insert("X-Playbox-Room-Id".into(), "drill1-room".into());
    corr.insert("X-Playbox-Run-Id".into(), "drill1-run".into());
    corr.insert("X-Playbox-Member-Id".into(), "w0".into());
    corr.insert("X-Playbox-Cache-Mode".into(), "off".into());
    corr.insert("X-Playbox-Failover-Policy".into(), "same_model_only".into());

    let box7 = Proxy::start_with_host_and_headers(
        &cfg,
        &secrets,
        &["llm".to_string(), "llm_tiny".to_string()],
        Ipv4Addr::LOCALHOST,
        "127.0.0.1",
        &corr,
    )
    .await
    .unwrap();

    // 6. Assertion A: Request 1 hits ch1 (429 Retry-After: 1200) and fails over to ch2 cleanly.
    let req_body = serde_json::to_vec(&serde_json::json!({
        "model": "gpt-drill",
        "stream": true,
        "messages": [{"role": "user", "content": "ping"}]
    }))
    .unwrap();
    let t1 = Instant::now();
    let r1 = http
        .post(format!("{}/v1/chat/completions", box7.base_url("llm")))
        .header("content-type", "application/json")
        .header("x-playbox-run-id", "SPOOFED-BY-GUEST")
        .body(req_body.clone())
        .send()
        .await
        .unwrap();
    let r1_status = r1.status().as_u16();
    let r1_text = r1.text().await.unwrap();
    let r1_ms = t1.elapsed().as_millis();
    assert_eq!(r1_status, 200, "req1 failed: {r1_text}");
    assert!(
        r1_text.contains("from ch2"),
        "req1 did not reach ch2: {r1_text}"
    );
    let ch1_after_r1 = ch1_hits.load(Ordering::SeqCst);
    assert!(
        ch1_after_r1 >= 1,
        "expected ch1 (higher priority) to be tried first"
    );
    assert!(
        ch2_hits.load(Ordering::SeqCst) >= 1,
        "expected failover to ch2"
    );
    eprintln!("Drill 1A (initial 429 failover): HTTP 200 in {r1_ms}ms (ch1_hits={ch1_after_r1})");

    // 7. Assertion B: Request 2 skips cooling-down ch1 completely (<100ms, ch1_hits unchanged!).
    let t2 = Instant::now();
    let r2 = http
        .post(format!("{}/v1/chat/completions", box7.base_url("llm")))
        .header("content-type", "application/json")
        .body(req_body)
        .send()
        .await
        .unwrap();
    let r2_status = r2.status().as_u16();
    let r2_text = r2.text().await.unwrap();
    let r2_ms = t2.elapsed().as_millis();
    assert_eq!(r2_status, 200, "req2 failed: {r2_text}");
    assert!(r2_text.contains("from ch2"));
    assert_eq!(
        ch1_hits.load(Ordering::SeqCst),
        ch1_after_r1,
        "ch1 must remain skipped while in 429 cooldown"
    );
    assert!(
        r2_ms < 250,
        "cooldown-skipped request should be fast (<250ms), took {r2_ms}ms"
    );
    eprintln!("Drill 1B (cooldown skip): HTTP 200 in {r2_ms}ms (ch1_hits stayed {ch1_after_r1})");

    // 8. Assertion C: $0.001-capped worker token cuts off RUNAWAY_LOOP mid-SSE stream!
    let runaway_body = serde_json::to_vec(&serde_json::json!({
        "model": "gpt-drill",
        "stream": true,
        "messages": [{"role": "user", "content": "RUNAWAY_LOOP"}]
    }))
    .unwrap();
    let r_tiny = http
        .post(format!("{}/v1/chat/completions", box7.base_url("llm_tiny")))
        .header("content-type", "application/json")
        .body(runaway_body)
        .send()
        .await
        .unwrap();
    let tiny_stop = r_tiny
        .headers()
        .get("x-playbox-stop-reason")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let tiny_text = r_tiny.text().await.unwrap();
    assert_eq!(
        tiny_stop.as_deref(),
        Some("budget_exhausted"),
        "expected X-Playbox-Stop-Reason: budget_exhausted on mid-SSE cutoff"
    );
    assert!(
        tiny_text.len() < 10_000,
        "expected mid-SSE truncation on $0.001 token, got {} bytes",
        tiny_text.len()
    );
    eprintln!(
        "Drill 1C (mid-SSE $0.001 cutoff): truncated to {} bytes (stop_reason={:?})",
        tiny_text.len(),
        tiny_stop
    );

    // 9. Assertion D: Real SQLite `playbox.llm_ledger.v1` populated by BOX-7 correlation headers
    //    (ignoring guest SPOOFED header) + bound into hybrid Ed25519+ML-DSA-65 receipt + offline verified!
    let mut ledger = serde_json::Value::Null;
    for _ in 0..40 {
        let (_, lr) = get_json(
            &http,
            &format!("{ap_base}/api/playbox/ledger?run_id=drill1-run"),
            admin_key,
        )
        .await;
        let reqs = lr["data"]["totals"]["requests"].as_u64().unwrap_or(0);
        if reqs >= 2 {
            ledger = lr["data"].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(
        ledger["totals"]["requests"].as_u64().unwrap_or(0) >= 2,
        "expected >=2 logged requests in SQLite ledger, got: {ledger}"
    );
    assert_eq!(ledger["room_id"], "drill1-room");
    assert_eq!(ledger["run_id"], "drill1-run");
    assert!(
        ledger["totals"]["failovers"].as_u64().unwrap_or(0) >= 1,
        "expected failover recorded in ledger: {ledger}"
    );
    assert!(ledger["totals"]["cost_usd"].as_f64().unwrap_or(0.0) > 0.0);

    let (_, spoof_lr) = get_json(
        &http,
        &format!("{ap_base}/api/playbox/ledger?run_id=SPOOFED-BY-GUEST"),
        admin_key,
    )
    .await;
    assert_eq!(
        spoof_lr["data"]["totals"]["requests"]
            .as_u64()
            .unwrap_or(99),
        0
    );

    let ledger_path = tmp.join("drill1.llm_ledger.json");
    let receipt_path = tmp.join("drill1.receipt.json");
    std::fs::write(&ledger_path, serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();

    let py_script = format!(
        r#"import json, sys
sys.path.insert(0, {backend:?})
from playbox_router import build_signed_receipt, verify_ledger, verify_receipt_ledger, verify_signed_receipt
ledger = json.loads(open({ledger_p:?}).read())
ok_l, why_l = verify_ledger(ledger)
assert ok_l, f"verify_ledger failed: {{why_l}}"
rcpt = build_signed_receipt(
    instance="drill1-wire-e2e",
    base_commit="0000000",
    runtime="box7-wire",
    git_history_isolated=True,
    host_audit_verified=True,
    box7_egress_hosts=["llm"],
    llm_ledger=ledger,
)
open({rcpt_p:?}, "w").write(json.dumps(rcpt, indent=2))
ok_r, msg_r = verify_signed_receipt(rcpt, require_pq=True)
assert ok_r, f"verify_signed_receipt(require_pq=True) failed: {{msg_r}}"
ok_m, msg_m = verify_receipt_ledger(rcpt, ledger)
assert ok_m, f"verify_receipt_ledger failed: {{msg_m}}"
print("PY_RECEIPT_OK:", msg_r, "cost_usd=", rcpt["payload"]["llm_ledger"]["totals"]["cost_usd"])
"#,
        backend = root.join("hermes-playbox-backend").to_string_lossy(),
        ledger_p = ledger_path.to_string_lossy(),
        rcpt_p = receipt_path.to_string_lossy(),
    );
    let py_out = Command::new("python3")
        .args(["-c", &py_script])
        .output()
        .expect("run python3 receipt verification");
    assert!(
        py_out.status.success(),
        "Python PQ receipt verification failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&py_out.stdout),
        String::from_utf8_lossy(&py_out.stderr)
    );
    eprintln!(
        "Drill 1D: {}",
        String::from_utf8_lossy(&py_out.stdout).trim()
    );

    if let Some(pb) = find_playbox_bin(&root) {
        let vout = Command::new(&pb)
            .args([
                "gate",
                "verify",
                receipt_path.to_str().unwrap(),
                "--require-signature",
                "--require-pq",
                "--json",
            ])
            .output()
            .expect("run playbox gate verify");
        assert!(
            vout.status.success(),
            "playbox gate verify failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&vout.stdout),
            String::from_utf8_lossy(&vout.stderr)
        );
        let vj: serde_json::Value = serde_json::from_slice(&vout.stdout).unwrap();
        assert_eq!(vj["valid"], true, "gate verify json not valid: {vj}");
        assert_eq!(vj["ed25519_verified"], true, "ed25519 not verified: {vj}");
        assert_eq!(vj["pq_verified"], true, "ml-dsa-65 not verified: {vj}");
        eprintln!("Drill 1E (`playbox gate verify --require-signature --require-pq`): {vj}");
    }

    let _ = std::fs::remove_dir_all(&tmp);
}
