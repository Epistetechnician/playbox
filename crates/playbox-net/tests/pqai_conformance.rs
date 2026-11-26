//! End-to-end Layer 1 + Layer 3 PQAI conformance test:
//! 1. Seeds a real governed room with `pqai-audit` and `pqai-migrate` templates and verifies
//!    fail-closed zero-egress limits, owner-gated memory review, Canvas protocol seed, and capabilities.
//! 2. Verifies Layer 1 FIPS 205 `SLH-DSA-SHA2-128f` Biscuit root anchor + tamper rejection.
//! 3. Verifies Layer 1 `RoomLog` tri-signed (`Ed25519 + ML-DSA-65 + SLH-DSA-SHA2-128f`) epoch
//!    checkpoint quorum (`committed_pqai`) and fail-closed rejection of tampered hash signatures.

use ed25519_dalek::SigningKey;
use playbox_core::pq;
use playbox_core::roomlog::{Config, RoomLog};
use playbox_net::grants::Authority;
use playbox_net::templates::{seed_room, TemplateKind};
use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

#[tokio::test]
async fn pqai_templates_zero_egress_and_slhdsa_roomlog_and_biscuit_anchor() {
    // 1. Verify air-gapped PQAI templates and seed a real room.
    for kind in [TemplateKind::PqaiAudit, TemplateKind::PqaiMigrate] {
        let spec = kind.spec();
        assert_eq!(TemplateKind::parse(spec.id), Some(kind));
        assert!(!spec.limits.network, "{} must disable network", spec.id);
        assert!(spec.limits.allowed_hosts.is_empty());
        assert!(spec.limits.allowed_egress.is_empty());
        assert!(spec.limits.allowed_secrets.is_empty());
        assert!(
            spec.memory_review,
            "{} must require owner memory review",
            spec.id
        );
        assert!(spec
            .capabilities
            .iter()
            .any(|c| c.name == "pqai-scan" || c.name == "pqai-fuzz"));
        assert!(spec
            .capabilities
            .iter()
            .any(|c| c.name == "gate-verify-pqai" && c.approval));
    }

    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default())
        .await
        .expect("start pqai-audit host");
    seed_room(&host, TemplateKind::PqaiAudit).await;
    let link = host
        .create_link(Duration::from_secs(60), 2)
        .unwrap()
        .encode();
    let guest = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .expect("guest joins pqai-audit room");
    let feed = guest.feed(0).await.unwrap();
    assert!(feed
        .items
        .iter()
        .any(|i| i.text.contains("pqai-audit-charter")));
    drop(guest);
    host.shutdown().await.expect("shutdown host");

    // 2. Verify Biscuit root public-key + revocation-set anchor signed with FIPS 205 SLH-DSA-SHA2-128f.
    let auth = Authority::new("pqai-audit-room");
    let mut revoked = HashSet::new();
    revoked.insert("compromised-agent-1".to_string());
    let anchor = auth
        .anchor_root_pqai(1, &revoked)
        .expect("issue SLH-DSA Biscuit root anchor");
    assert!(auth.verify_root_anchor_pqai(&anchor, &revoked, Some(&anchor.slh_pk)));

    // Tampered revocation set or epoch must fail closed.
    let mut missing_revoked = HashSet::new();
    missing_revoked.insert("other-agent".to_string());
    assert!(!auth.verify_root_anchor_pqai(&anchor, &missing_revoked, Some(&anchor.slh_pk)));
    let mut bad_epoch = anchor.clone();
    bad_epoch.epoch += 1;
    assert!(!auth.verify_root_anchor_pqai(&bad_epoch, &revoked, Some(&anchor.slh_pk)));

    // 3. Verify RoomLog tri-signed checkpoint commitment (`committed_pqai`) and tamper detection.
    let root_sk = SigningKey::from_bytes(&[1u8; 32]);
    let h1 = SigningKey::from_bytes(&[2u8; 32]);
    let h2 = SigningKey::from_bytes(&[3u8; 32]);
    let h1_pk = h1.verifying_key().to_bytes();
    let h2_pk = h2.verifying_key().to_bytes();
    let cfg = Config {
        epoch: 1,
        leader: h1_pk,
        hosts: BTreeSet::from([h1_pk, h2_pk]),
        k: 2,
    };
    let log = RoomLog::genesis(&root_sk, cfg, 1_700_000_000_000).expect("roomlog genesis");
    let cp = log.checkpoint();
    let mut sc = log
        .sign_checkpoint_pqai(&h1, cp.clone())
        .expect("h1 pqai sign");
    let sc2 = log.sign_checkpoint_pqai(&h2, cp).expect("h2 pqai sign");
    assert_eq!(log.merge(&mut sc, &sc2), 3);
    assert_eq!(log.committed(&[sc.clone()]), 1);
    assert_eq!(log.committed_pq(&[sc.clone()]), 1);
    assert_eq!(log.committed_pqai(&[sc.clone()]), 1);

    // Flip 1 byte in h1's SLH-DSA hash signature: classical + ML-DSA-65 still hold, but pqai_committed drops to 0.
    let mut tampered = sc.clone();
    tampered.hash_sigs.get_mut(&h1_pk).unwrap().sig[0] ^= 0xff;
    assert_eq!(log.committed(&[tampered.clone()]), 1);
    assert_eq!(log.committed_pq(&[tampered.clone()]), 1);
    assert_eq!(
        log.committed_pqai(&[tampered]),
        0,
        "tampered SLH-DSA checkpoint sig must drop pqai_committed to 0"
    );

    // Direct FIPS 205 SLH-DSA roundtrip and context binding.
    let (slh_pk, slh_sk) = pq::derive_slhdsa_from_ed25519_seed(&[0x42u8; 32]);
    let sig = pq::sign_slhdsa(&slh_sk, b"pqai-root", b"ctx-a").unwrap();
    assert!(pq::verify_slhdsa(&slh_pk, b"pqai-root", &sig, b"ctx-a"));
    assert!(!pq::verify_slhdsa(&slh_pk, b"pqai-root", &sig, b"ctx-b"));
}
