//! Phase 4 end-to-end black-box conformance test for the `arc-rsi` sovereign template:
//!
//! 1. Real 2-node `iroh` room (`Host` + Proposer `A` + Verifier `B`) seeded with `TemplateKind::ArcRsi`.
//! 2. Native Rust `BOX-7` (`egress::Proxy::start_with_arc_maverick`) enforcing fail-closed `403`,
//!    CPU-ARC local hit bypass (`0` upstream egress), cache-miss forwarding, and 1-RTT Goldilocks
//!    Maverick Verification-Only (honest `200` + 1-LSB cheat `502`).
//! 3. 64-byte int8 Swarm Telepathy skill broadcast (`PBL2`) with novelty filter & signed receipt commit.
//! 4. Zero unsigned promotions enforced by `RoomLog` + offline verification of dual-signed
//!    (`Ed25519 + ML-DSA-65`) checkpoints (`committed_pq`).

use ed25519_dalek::SigningKey;
use playbox_core::promotion::{
    promotion_payload, register_payload, Promotion, Receipt, Register, Rule,
};
use playbox_core::roomlog::{Config, LogError, RoomLog};
use playbox_net::latent::{commit_of, pack_with};
use playbox_net::templates::{seed_room, TemplateKind};
use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use playbox_runner::egress::{EgressConfig, EgressUpstream, Proxy};
use playbox_runner::maverick::{
    encode_skill_descriptor_64b, is_novel_descriptor, ArcMaverickHook, CpuArcSkill,
    MaverickVerifyRequest,
};
use playbox_runner::secrets::SecretStore;
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const HUB: &str = "a1b2c3d4e5f60718";

async fn spawn_fake_upstream() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_bg = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            hits_bg.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf).await;
            let body = r#"{"id":"upstream-1","choices":[{"message":{"content":"from-upstream"}}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes()).await;
            let _ = s.shutdown().await;
        }
    });
    (port, hits)
}

#[tokio::test]
async fn arc_rsi_two_node_blackbox_conformance() {
    // 1. Start a real 2-node iroh room seeded with `arc-rsi` template.
    let spec = TemplateKind::ArcRsi.spec();
    assert_eq!(spec.id, "arc-rsi");
    assert_eq!(TemplateKind::parse("arc-rsi"), Some(TemplateKind::ArcRsi));
    assert_eq!(spec.default_runtime, "krun");
    assert!(spec.memory_review);
    assert!(spec
        .capabilities
        .iter()
        .any(|c| c.name == "promote" && c.approval));

    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default())
        .await
        .unwrap();
    seed_room(&host, TemplateKind::ArcRsi).await;
    let link = host
        .create_link(Duration::from_secs(60), 2)
        .unwrap()
        .encode();

    let secret_a = new_secret_key().unwrap();
    let key_a = SigningKey::from_bytes(&secret_a.to_bytes());
    let client_a = join_link(&link, secret_a, true).await.unwrap();

    let secret_b = new_secret_key().unwrap();
    let key_b = SigningKey::from_bytes(&secret_b.to_bytes());
    let client_b = join_link(&link, secret_b, true).await.unwrap();

    // Verify room canvas received the arc-rsi protocol artifact.
    let feed = client_b.feed(0).await.unwrap();
    assert!(feed
        .items
        .iter()
        .any(|i| i.text.contains("arc-rsi-protocol")));

    // 2. Wire native BOX-7 Proxy with ArcMaverickHook in front of a local upstream.
    let (upstream_port, upstream_hits) = spawn_fake_upstream().await;
    let td = std::env::temp_dir().join(format!("pb-arc-rsi-conformance-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&td);
    let sec_dir = td.join("secrets");
    std::fs::create_dir_all(&sec_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sec_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let key_path = sec_dir.join("ai_key");
        std::fs::write(&key_path, "sk-test-secret-key").unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let secrets = SecretStore::new(&sec_dir);

    let cfg = EgressConfig {
        upstreams: vec![EgressUpstream {
            name: "aiproxy".into(),
            base: format!("http://127.0.0.1:{upstream_port}"),
            secret: "ai_key".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            allow_paths: vec!["/v1/chat/completions".into(), "/v1/maverick/verify".into()],
            pass_headers: vec!["content-type".into()],
            extra_headers: BTreeMap::new(),
            env_var: None,
            insecure_http: true,
            allow_private: true,
        }],
        log_file: None,
        max_log_bytes: None,
    };

    let root_key = SigningKey::from_bytes(&[7u8; 32]);
    let leader_key = SigningKey::from_bytes(&[9u8; 32]);
    let hook = Arc::new(ArcMaverickHook::new(&root_key, leader_key));
    let skill = hook.register_skill(CpuArcSkill::compile(
        "compute 7*6",
        r#"{"id":"arc-local","choices":[{"message":{"content":"42"}}]}"#,
    ));

    let proxy = Proxy::start_with_arc_maverick(
        &cfg,
        &secrets,
        &["aiproxy".into()],
        Ipv4Addr::LOCALHOST,
        "127.0.0.1",
        &BTreeMap::new(),
        Some(Arc::clone(&hook)),
    )
    .await
    .unwrap();
    let base = proxy.base_url("aiproxy");
    let http = reqwest::Client::new();

    // 2a. Fail-closed 403 on unauthorized path and missing capability token.
    let r_unauth = http
        .get(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            proxy.port()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r_unauth.status().as_u16(), 403);
    let r_path = http
        .get(format!("{base}/admin/secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(r_path.status().as_u16(), 403);

    // 2b. CPU-ARC local hit: 200 OK, 0 upstream calls, 64B descriptor header present.
    let r_hit = http
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"messages":[{"role":"user","content":"please compute 7*6 now"}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r_hit.status().as_u16(), 200);
    assert_eq!(
        r_hit
            .headers()
            .get("x-playbox-arc-hit")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
    let latent_hex = r_hit
        .headers()
        .get("x-playbox-latent-64b")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert_eq!(latent_hex.len(), 128);
    assert!(r_hit.text().await.unwrap().contains("42"));
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 0);

    // 2c. Cache miss forwards cleanly to upstream.
    let r_miss = http
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"messages":[{"role":"user","content":"unseen prompt"}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r_miss.status().as_u16(), 200);
    assert!(r_miss.text().await.unwrap().contains("from-upstream"));
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);

    // 2d. 1-RTT Maverick Goldilocks Verification-Only: honest 200 vs 1-LSB cheat 502.
    let d_out = 8usize;
    let d_in = 12usize;
    let w_mat: Vec<i64> = (0..d_out * d_in).map(|i| (i as i64 % 11) - 5).collect();
    let x: Vec<i64> = (0..d_in).map(|i| (i as i64 % 7) - 3).collect();
    let mut y = vec![0i64; d_out];
    for i in 0..d_out {
        for j in 0..d_in {
            y[i] += w_mat[i * d_in + j] * x[j];
        }
    }
    let honest_req = MaverickVerifyRequest {
        d_out,
        d_in,
        w_row_major: w_mat.clone(),
        x: x.clone(),
        claimed_y: y.clone(),
        challenge_seed: 2026,
        repetitions: 4,
    };
    let r_ok = http
        .post(format!("{base}/v1/maverick/verify"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&honest_req).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r_ok.status().as_u16(), 200);
    assert_eq!(
        r_ok.headers()
            .get("x-playbox-maverick-verified")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );

    let mut cheated_y = y;
    cheated_y[5] += 1; // 1-LSB cheat
    let cheat_req = MaverickVerifyRequest {
        d_out,
        d_in,
        w_row_major: w_mat,
        x,
        claimed_y: cheated_y,
        challenge_seed: 2026,
        repetitions: 4,
    };
    let r_cheat = http
        .post(format!("{base}/v1/maverick/verify"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&cheat_req).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r_cheat.status().as_u16(), 502);
    assert_eq!(
        r_cheat
            .headers()
            .get("x-playbox-maverick-cheat")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
    assert_eq!(hook.stats(), (1, 1, 1, 1));

    // 3. Offline verification of dual-signed (Ed25519 + ML-DSA-65) RoomLog receipts from BOX-7.
    let (room_pk, entries, checkpoints) = hook.export_log_and_checkpoints();
    let mut follower = RoomLog::follower(room_pk);
    follower.accept_all(entries).unwrap();
    let verified_log = follower.log().unwrap().clone();
    assert_eq!(verified_log.committed(&checkpoints), verified_log.len());
    assert_eq!(verified_log.committed_pq(&checkpoints), verified_log.len());

    // 4. 64-byte int8 Swarm Telepathy skill broadcast with novelty filter over real iroh room.
    let prior = vec![encode_skill_descriptor_64b("prior-skill", "prior-out")];
    assert!(!is_novel_descriptor(&prior, &prior[0], 0.90));
    assert!(is_novel_descriptor(&prior, &skill.descriptor_64b, 0.90));

    let receipt_sentence = format!(
        "skill_hash={} room_head={}",
        skill.skill_hash,
        playbox_core::roomlog::hex(&verified_log.head())
    );
    let salt_hex = "0102030405060708090a0b0c0d0e0f10";
    let commit = commit_of(salt_hex, &[receipt_sentence]);
    let env = pack_with(
        &key_a,
        HUB,
        "cpu-arc-v1",
        0.01,
        &skill.descriptor_64b,
        Some(&commit),
    );
    let seq = client_a.latent_send(&env).await.unwrap();
    assert_eq!(seq, 1);

    let (pulled, _) = client_b.latent_pull(HUB, 0).await.unwrap();
    assert_eq!(pulled.len(), 1);
    assert_eq!(pulled[0].envelope.body.as_slice(), &skill.descriptor_64b);
    assert_eq!(pulled[0].envelope.commit.as_deref(), Some(commit.as_str()));

    // 5. Zero unsigned promotions + 2-host (j=2) quorum promotion verified offline with Ed25519+ML-DSA-65.
    let pk_a = key_a.verifying_key().to_bytes();
    let pk_b = key_b.verifying_key().to_bytes();
    let mut hosts = BTreeSet::new();
    hosts.insert(pk_a);
    hosts.insert(pk_b);
    let mut promo_log = RoomLog::genesis(
        &root_key,
        Config {
            epoch: 0,
            leader: pk_a,
            hosts,
            k: 2,
        },
        1_000,
    )
    .unwrap();
    let problem = *blake3::hash(b"math500-sealed-n400").as_bytes();
    let candidate = *blake3::hash(b"qr-nullspace-delta-v1").as_bytes();
    let weights = *blake3::hash(b"qwen3-30b-a3b-bf16").as_bytes();
    let env_hash = *blake3::hash(b"gcp-intel-tdx-c3").as_bytes();
    let reg = Register {
        problem,
        rule: Rule {
            j: 2,
            noise_ppm: 10_000,
            min_gain_ppm: 50_000,
        },
        baseline: 397_500,
    };
    promo_log
        .append(&key_a, "problem.register", &register_payload(&reg), 1_001)
        .unwrap();

    // 5a. Forged/unsigned single-host promotion MUST fail (zero unsigned promotions).
    let single_receipt = Receipt::sign(
        &promo_log.room(),
        &key_a,
        &problem,
        &candidate,
        &weights,
        env_hash,
        3,
        620_000,
    );
    let unsigned_promo = Promotion {
        problem,
        candidate,
        weights,
        receipts: vec![single_receipt.clone()],
    };
    let err = promo_log
        .append(
            &key_a,
            "promotion",
            &promotion_payload(&unsigned_promo),
            1_002,
        )
        .unwrap_err();
    assert!(matches!(err, LogError::Promotion(_, _)));

    // 5b. Valid 2-host T3-attested quorum promotion succeeds and commits with hybrid Ed25519+ML-DSA-65.
    let receipt_b = Receipt::sign(
        &promo_log.room(),
        &key_b,
        &problem,
        &candidate,
        &weights,
        env_hash,
        3,
        621_000,
    );
    let valid_promo = Promotion {
        problem,
        candidate,
        weights,
        receipts: vec![single_receipt, receipt_b],
    };
    promo_log
        .append(&key_a, "promotion", &promotion_payload(&valid_promo), 1_003)
        .unwrap();

    let cp = promo_log.checkpoint();
    let mut sc_a = promo_log.sign_checkpoint(&key_a, cp.clone()).unwrap();
    let sc_b = promo_log.sign_checkpoint(&key_b, cp).unwrap();
    promo_log.merge(&mut sc_a, &sc_b);

    // Offline replay with only root public key verifies quorum + ML-DSA-65 + frontier strength T3.
    let mut offline = RoomLog::follower(promo_log.room());
    offline.accept_all(promo_log.entries().to_vec()).unwrap();
    let replayed = offline.log().unwrap().clone();
    assert_eq!(replayed.committed_pq(&[sc_a]), replayed.len());
    let frontier = playbox_core::promotion::replay(replayed.entries());
    let (_, best) = frontier.get(&problem).unwrap();
    assert_eq!(best.candidate, candidate);
    assert_eq!(best.metric, 620_000);
    assert_eq!(best.strength, 3);

    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&td);
}
