//! Cross-crate security invariant regression tests from the Track 3 security audit.

use ed25519_dalek::SigningKey;
use playbox_core::{
    receipt::{self, Coverage, Level},
    record, roomlog, witness,
};
use playbox_net::{
    decode_peer,
    grants::{Authority, Caps},
    revoke::{Notice, MAX_NOTICE_BYTES},
    roomsync, Link, MAX_LINK_BYTES, MAX_PEER_BYTES,
};
use serde_json::json;
use std::collections::BTreeSet;

fn make_tar_entry(name: &str, kind: u8, link: &str, body: &[u8]) -> Vec<u8> {
    let mut h = [0u8; 512];
    let nb = name.as_bytes();
    h[..nb.len().min(100)].copy_from_slice(&nb[..nb.len().min(100)]);
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    let sz = format!("{:011o}\0", body.len());
    h[124..136].copy_from_slice(sz.as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[148..156].fill(b' ');
    h[156] = kind;
    let lb = link.as_bytes();
    h[157..157 + lb.len().min(100)].copy_from_slice(&lb[..lb.len().min(100)]);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let csum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let cs = format!("{csum:06o}\0 ");
    h[148..156].copy_from_slice(cs.as_bytes());
    let mut out = Vec::with_capacity(512 + ((body.len() + 511) & !511));
    out.extend_from_slice(&h);
    out.extend_from_slice(body);
    let rem = body.len() % 512;
    if rem != 0 {
        out.resize(out.len() + (512 - rem), 0);
    }
    out
}

#[test]
fn openshell_tar_blocks_pax_g_zero_smuggling_and_chained_symlink_escape() {
    // PAX global header override
    let mut g = make_tar_entry("global", b'g', "", b"23 path=../../.bashrc\n");
    g.extend(make_tar_entry("safe.txt", b'0', "", b"data"));
    g.extend([0u8; 1024]);
    assert!(playbox_runner::validate_openshell_tar_bytes(&g).is_err());

    // Single zero block followed by malicious entry
    let mut z = make_tar_entry("safe.txt", b'0', "", b"data");
    z.extend([0u8; 512]);
    z.extend(make_tar_entry("../../pwn", b'0', "", b"pwn"));
    z.extend([0u8; 1024]);
    assert!(playbox_runner::validate_openshell_tar_bytes(&z).is_err());

    // Chained symlink escape: a/b -> .., then a/b/c -> ../.., then a/b/c/out
    let mut s = make_tar_entry("a/b", b'2', "..", b"");
    s.extend(make_tar_entry("a/b/c", b'2', "../..", b""));
    s.extend(make_tar_entry("a/b/c/out", b'0', "", b"pwn"));
    s.extend([0u8; 1024]);
    assert!(playbox_runner::validate_openshell_tar_bytes(&s).is_err());
}

#[test]
fn witness_cosign_does_not_panic_on_size_zero_nonzero_head() {
    let host = SigningKey::from_bytes(&[1u8; 32]);
    let mut w = witness::Witness::new(SigningKey::from_bytes(&[2u8; 32]));
    let cp = record::sign(
        "playbox.checkpoint.v1",
        &[
            ("size", "0".into()),
            ("head", "ff".repeat(32)),
            ("time", "1".into()),
        ],
        &host,
    )
    .unwrap();
    assert!(matches!(
        w.cosign(&cp, &[]),
        Err(witness::Refusal::Inconsistent(_))
    ));
}

#[test]
fn roomlog_config_and_receipt_threshold_avoid_integer_overflow() {
    let leader = SigningKey::from_bytes(&[3u8; 32])
        .verifying_key()
        .to_bytes();
    let mut hosts = BTreeSet::new();
    hosts.insert(leader);
    let cfg = roomlog::Config {
        epoch: 0,
        leader,
        hosts,
        k: usize::MAX / 2 + 1,
    };
    assert!(cfg.check().is_err());

    let cov = Coverage {
        responses: 1,
        notarized: 1,
        ..Coverage::default()
    };
    assert!(receipt::require(&cov, Level::Notarized, 0, 0).is_err());
    assert!(receipt::require(&cov, Level::Notarized, usize::MAX, usize::MAX).is_ok());
}

#[test]
fn roomsync_load_ignores_zero_sig_injected_checkpoints() {
    let root = SigningKey::from_bytes(&[10u8; 32]);
    let host = SigningKey::from_bytes(&[11u8; 32]);
    let leader = host.verifying_key().to_bytes();
    let mut hosts = BTreeSet::new();
    hosts.insert(leader);
    let log = roomlog::RoomLog::genesis(
        &root,
        roomlog::Config {
            epoch: 0,
            leader,
            hosts,
            k: 1,
        },
        1,
    )
    .unwrap();
    let mut text = roomlog::text::to_text(&log);
    // Inject forged 0-sig checkpoint lines.
    for i in 1..=10u64 {
        let bogus = json!({
            "cert": {
                "cp": { "epoch": 0, "size": i, "head": "aa".repeat(32) },
                "sigs": {}
            }
        });
        text.push_str(&bogus.to_string());
        text.push('\n');
    }
    let st = roomsync::read_status_text(root.verifying_key().to_bytes(), &text)
        .unwrap()
        .expect("status");
    assert_eq!(
        st.committed, 0,
        "unverified zero-sig checkpoints must never commit entries"
    );
}

#[test]
fn revocation_link_and_grant_ttl_bounds_hold() {
    let auth = Authority::new("audit-room");
    // Extreme TTL must saturate rather than overflow i64.
    let (_inv, _) = auth.mint_invite(i64::MAX).unwrap();
    let (_grant, _) = auth
        .mint_grant(
            "subject",
            i64::MAX,
            &Caps {
                cpu_milli: 1000,
                mem_mib: 256,
                timeout_secs: 60,
                net: false,
                hosts: vec![],
            },
        )
        .unwrap();

    // Oversized revocation notice rejected immediately.
    assert!(Notice::parse(&"x".repeat(MAX_NOTICE_BYTES + 1), &auth).is_err());

    // Oversized link / peer and control characters in invite rejected.
    assert!(Link::parse(&format!("playbox://{}#ok", "a".repeat(MAX_LINK_BYTES))).is_err());
    assert!(decode_peer(&"a".repeat(MAX_PEER_BYTES + 1)).is_err());
    assert!(Link::parse("playbox://e30#bad\rinvite").is_err());
}
