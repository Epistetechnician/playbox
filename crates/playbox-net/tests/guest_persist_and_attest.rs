//! Integration tests for Slice 1 (persistent guest grant vault + auto-resume)
//! and Slice 2 (mutual guest attestation with --min-guest-tier & unspent invite on failure).

use playbox_core::attest::{
    AttestError, Measurements, PlatformProvider, PlatformVerdict, PlatformVerifier, Produced,
    Statement, Tier,
};
use playbox_net::{
    encode_peer, grants, new_secret_key, wire, Client, GuestStore, Host, HostConfig, Link,
    SavedRoom,
};
use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

/// Mock hardware provider that binds the 32-byte statement digest, proving end-to-end
/// platform evidence transport and verification without requiring physical hardware in CI.
struct MockHwProvider {
    device_fp: &'static str,
}

impl PlatformProvider for MockHwProvider {
    fn kind(&self) -> &'static str {
        "mock-hw"
    }

    fn produce(&self, binding: &[u8; 32], _: &Measurements) -> Result<Produced, AttestError> {
        let hex: String = binding.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Produced {
            evidence: format!("bound {hex}\ndevice {}\n", self.device_fp),
            claimed: Tier::T1,
        })
    }
}

struct MockHwVerifier;

impl PlatformVerifier for MockHwVerifier {
    fn kind(&self) -> &'static str {
        "mock-hw"
    }

    fn verify(&self, st: &Statement) -> Result<PlatformVerdict, AttestError> {
        let ev = String::from_utf8(
            (0..st.platform_evidence.len() / 2)
                .map(|i| u8::from_str_radix(&st.platform_evidence[i * 2..i * 2 + 2], 16).unwrap())
                .collect(),
        )
        .unwrap();
        let expected: String = st.binding().iter().map(|b| format!("{b:02x}")).collect();
        let mut bound = None;
        let mut device = None;
        for line in ev.lines() {
            if let Some(v) = line.strip_prefix("bound ") {
                bound = Some(v.trim());
            } else if let Some(v) = line.strip_prefix("device ") {
                device = Some(v.trim().to_string());
            }
        }
        if bound != Some(expected.as_str()) {
            return Err("mock-hw binding mismatch".into());
        }
        Ok(PlatformVerdict {
            tier: Tier::T1,
            notes: vec!["mock-hw verified".into()],
            challenge: None,
            device,
        })
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "pbx-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn guest_grant_persists_and_auto_resumes_without_new_invite() {
    let dir = temp_dir("persist-resume");
    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default())
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(300), 1)
        .unwrap()
        .encode();
    let parsed = Link::parse(&link).unwrap();
    let guest_key = new_secret_key().unwrap();

    // First session joins and persists its grant into GuestStore.
    let c1 = Client::connect(parsed.addr.clone(), guest_key.clone(), true)
        .await
        .unwrap();
    let outcome = c1.join_details(&parsed.invite).await.unwrap();
    assert!(outcome.expires.is_some());
    let saved = SavedRoom {
        host_id: c1.host_id().to_string(),
        addr: encode_peer(c1.host_addr()),
        transport: c1.transport().label(),
        grant: outcome.grant.clone(),
        member_id: outcome.member_id.clone(),
        joined_at: grants::now_secs(),
        expires_at: outcome.expires,
        hint_hosts: vec![],
        min_host_tier: "t0".into(),
        trust_host_device: false,
        workspace: None,
        handle: None,
    };
    let vault = GuestStore::new(&dir);
    vault.save(&saved).unwrap();
    c1.say("before restart", None).await.unwrap();
    c1.close().await;

    // Verify file permissions are owner-only (0600).
    let mode = std::fs::metadata(dir.join(format!("guest-rooms/{}.json", saved.host_id)))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    // Second session (after restart) loads the vault and resumes with the same grant—no new invite!
    let loaded = vault.load_active().unwrap().expect("active room saved");
    assert!(!loaded.is_expired(grants::now_secs()));
    let c2 = Client::resume(
        loaded.host_addr().unwrap(),
        guest_key.clone(),
        loaded.parsed_transport().unwrap(),
        loaded.grant.clone(),
    )
    .await
    .unwrap();
    c2.echo("resumed").await.unwrap();
    c2.say("after restart", None).await.unwrap();

    // Once revoked by host, the persisted grant is rejected.
    assert!(host.revoke_member(&c2.id()));
    let err = c2.echo("after revoke").await.unwrap_err();
    assert!(
        format!("{err:#}").to_lowercase().contains("revoked")
            || format!("{err:#}").to_lowercase().contains("unauthorized"),
        "expected revocation error, got: {err:#}"
    );

    c2.close().await;
    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mutual_guest_attestation_t0_succeeds_and_records_tier() {
    let pins_dir = temp_dir("guest-t0");
    let host = Host::start_with(
        new_secret_key().unwrap(),
        true,
        HostConfig {
            min_guest_tier: Some(Tier::T0),
            guest_pins_home: Some(pins_dir.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let link = host
        .create_link(Duration::from_secs(300), 1)
        .unwrap()
        .encode();
    let parsed = Link::parse(&link).unwrap();
    let client = Client::connect(parsed.addr, new_secret_key().unwrap(), true)
        .await
        .unwrap();

    let outcome = client
        .join_with_provider(&parsed.invite, None)
        .await
        .unwrap();
    assert_eq!(outcome.guest_verified_tier.as_deref(), Some("T0"));
    client.echo("hello attested").await.unwrap();

    client.close().await;
    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&pins_dir);
}

#[tokio::test]
async fn failed_guest_attestation_leaves_invite_unspent_then_t1_guest_joins() {
    let pins_dir = temp_dir("guest-t1-unspent");
    let host = Host::start_with(
        new_secret_key().unwrap(),
        true,
        HostConfig {
            min_guest_tier: Some(Tier::T1),
            trust_guest_device: true,
            guest_pins_home: Some(pins_dir.clone()),
            guest_verifiers: vec![Arc::new(MockHwVerifier)],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // Single-use invite!
    let link = host
        .create_link(Duration::from_secs(300), 1)
        .unwrap()
        .encode();
    let parsed = Link::parse(&link).unwrap();

    // 1. Guest without hardware provider only achieves T0 -> must be refused, invite MUST stay unspent.
    let weak_guest = Client::connect(parsed.addr.clone(), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let err = weak_guest
        .join_with_provider(&parsed.invite, None)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("T1") && msg.contains("not spent"),
        "expected T1 refusal mentioning unspent invite, got: {msg}"
    );
    weak_guest.close().await;

    // 2. Guest with hardware provider joins using the EXACT SAME single-use invite!
    let strong_guest = Client::connect(parsed.addr.clone(), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let hw = MockHwProvider {
        device_fp: "aaaa1111",
    };
    let outcome = strong_guest
        .join_with_provider(&parsed.invite, Some(&hw))
        .await
        .unwrap();
    assert_eq!(outcome.guest_verified_tier.as_deref(), Some("T1"));
    assert_eq!(outcome.guest_device.as_deref(), Some("mock-hw:aaaa1111"));
    strong_guest.echo("t1 admitted").await.unwrap();
    strong_guest.close().await;

    // 3. Invite is now actually spent—a third guest cannot reuse it.
    let late_guest = Client::connect(parsed.addr, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let spent_err = late_guest
        .join_with_provider(&parsed.invite, Some(&hw))
        .await
        .unwrap_err();
    assert!(
        format!("{spent_err:#}").contains("already used"),
        "expected spent invite error, got: {spent_err:#}"
    );
    late_guest.close().await;
    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&pins_dir);
}

#[tokio::test]
async fn tampered_or_replayed_guest_attestation_fails_without_spending_invite() {
    let pins_dir = temp_dir("guest-tamper-replay");
    let host = Host::start_with(
        new_secret_key().unwrap(),
        true,
        HostConfig {
            min_guest_tier: Some(Tier::T0),
            guest_pins_home: Some(pins_dir.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let link = host
        .create_link(Duration::from_secs(300), 1)
        .unwrap()
        .encode();
    let parsed = Link::parse(&link).unwrap();

    let guest = Client::connect(parsed.addr.clone(), new_secret_key().unwrap(), true)
        .await
        .unwrap();

    // 1. Missing attestation when required -> fails without spending invite.
    let err = guest
        .call(
            wire::METHOD_JOIN,
            serde_json::json!({
                "invite": parsed.invite,
                "protocol": wire::PROTOCOL,
                "version": env!("CARGO_PKG_VERSION"),
            }),
        )
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("not spent"));

    // 2. Statement signed for a wrong nonce -> fails without spending invite (and consumes the challenge).
    let _ch = guest.join_challenge().await.unwrap();
    let wrong_nonce_stmt = guest.sign_guest_attestation("deadbeef", None).unwrap();
    let err = guest
        .call(
            wire::METHOD_JOIN,
            serde_json::json!({
                "invite": parsed.invite,
                "attestation": wrong_nonce_stmt,
                "protocol": wire::PROTOCOL,
                "version": env!("CARGO_PKG_VERSION"),
            }),
        )
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("nonce mismatch"));

    // 3. Replaying the same statement without requesting a fresh challenge -> fails without spending invite.
    let err = guest
        .call(
            wire::METHOD_JOIN,
            serde_json::json!({
                "invite": parsed.invite,
                "attestation": wrong_nonce_stmt,
                "protocol": wire::PROTOCOL,
                "version": env!("CARGO_PKG_VERSION"),
            }),
        )
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("missing or expired"));

    // 4. Statement signed by a different key than the connecting peer -> fails without spending invite.
    let ch2 = guest.join_challenge().await.unwrap();
    let imposter = Client::connect(parsed.addr.clone(), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let imposter_stmt = imposter.sign_guest_attestation(&ch2.nonce, None).unwrap();
    let err = guest
        .call(
            wire::METHOD_JOIN,
            serde_json::json!({
                "invite": parsed.invite,
                "attestation": imposter_stmt,
                "protocol": wire::PROTOCOL,
                "version": env!("CARGO_PKG_VERSION"),
            }),
        )
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("different key"));
    imposter.close().await;

    // 5. After all those failed attempts, the single-use invite is STILL unspent and succeeds cleanly!
    let outcome = guest
        .join_with_provider(&parsed.invite, None)
        .await
        .unwrap();
    assert_eq!(outcome.guest_verified_tier.as_deref(), Some("T0"));
    guest.close().await;
    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&pins_dir);
}
