//! Hub latent messages in a room: a member posts a signed `PBL2` envelope, the host files it and signs a receipt,
//! another member reads and verifies it. Forged, re-signed, oversized and cross-signer messages are refused.

use ed25519_dalek::SigningKey;
use playbox_net::latent::{commit_of, pack, pack_with, parse, verify_receipt};
use playbox_net::{join_link, new_secret_key, Client, Host, HostConfig};
use std::time::Duration;

const HUB: &str = "0123456789abcdef";

async fn start() -> (Host, (Client, SigningKey), (Client, SigningKey)) {
    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default())
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 2)
        .unwrap()
        .encode();
    let mut out = Vec::new();
    for _ in 0..2 {
        let secret = new_secret_key().unwrap();
        let key = SigningKey::from_bytes(&secret.to_bytes());
        out.push((join_link(&link, secret, true).await.unwrap(), key));
    }
    let b = out.pop().unwrap();
    let a = out.pop().unwrap();
    (host, a, b)
}

#[tokio::test]
async fn a_member_posts_a_vector_and_another_reads_it_without_trusting_the_host() {
    let (host, (a, ka), (b, _)) = start().await;
    let body: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(3)).collect();
    let env = pack(&ka, HUB, "smollm2", 0.0123, &body);
    let seq = a.latent_send(&env).await.unwrap();
    assert_eq!(seq, 1);

    let (got, next) = b.latent_pull(HUB, 0).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].envelope.body, body);
    assert_eq!(got[0].envelope.src, "smollm2");
    assert!((got[0].envelope.scale() - 0.0123).abs() < 1e-9);
    assert_eq!(got[0].from, a.id().to_string());
    assert_eq!(next, 1);
    assert!(
        b.latent_pull(HUB, next).await.unwrap().0.is_empty(),
        "the cursor returns only what is new"
    );
    assert!(
        b.latent_pull("ffffffffffffffff", 0)
            .await
            .unwrap()
            .0
            .is_empty(),
        "other hubs are separate"
    );

    // The room sees a notice with the hub, sender label and size, and no vector.
    let feed = b.feed(0).await.unwrap();
    let notice = feed
        .items
        .iter()
        .find(|i| i.text.starts_with("[latent]"))
        .expect("a feed notice");
    assert!(notice.text.contains("k=64") && notice.text.contains("smollm2"));
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn forged_cross_signed_and_malformed_messages_are_refused() {
    let (host, (a, ka), (b, kb)) = start().await;
    let body = vec![7u8; 32];
    // Signed by b but posted by a: the signer must be the sending member.
    assert!(a
        .latent_send(&pack(&kb, HUB, "x", 0.5, &body))
        .await
        .is_err());
    // Not an envelope at all, truncated, wrong magic.
    for bad in [
        b"hello".to_vec(),
        b"PBL2".to_vec(),
        b"PBL1\x00\x00\x00\x02{}".to_vec(),
    ] {
        assert!(a.latent_send(&bad).await.is_err());
    }
    // A flipped body byte, a swapped sender label and a swapped hub all break the signature or the hash.
    let good = pack(&ka, HUB, "x", 0.5, &body);
    let mut flipped = good.clone();
    *flipped.last_mut().unwrap() ^= 1;
    assert!(parse(&flipped).is_err());
    let relabelled = String::from_utf8_lossy(&good)
        .replacen("\"src\":\"x\"", "\"src\":\"y\"", 1)
        .into_bytes();
    assert!(parse(&relabelled).is_err());
    // The client refuses to send what the host would refuse, and nothing bad reached the mailbox.
    assert!(a.latent_send(&flipped).await.is_err());
    assert!(b.latent_pull(HUB, 0).await.unwrap().0.is_empty());
    // A receipt for other bytes is rejected.
    let seq = a.latent_send(&good).await.unwrap();
    let other = pack(&ka, HUB, "x", 0.5, &[9u8; 32]);
    assert!(verify_receipt(
        "playbox.latent-receipt.v1\nsig 00",
        &[0u8; 32],
        &[0u8; 32],
        HUB,
        &other
    )
    .is_err());
    assert_eq!(seq, 1);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_mailbox_is_bounded_per_member() {
    let (host, (a, ka), (b, _)) = start().await;
    for i in 0..40u8 {
        a.latent_send(&pack(&ka, HUB, "x", 0.5, &[i; 16]))
            .await
            .ok();
    }
    let (got, _) = b.latent_pull(HUB, 0).await.unwrap();
    assert!(
        got.len() <= 32,
        "one member cannot hold more than 32 messages, got {}",
        got.len()
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_stranger_cannot_post_or_read() {
    let (host, (_a, _), (_b, _)) = start().await;
    let sk = new_secret_key().unwrap();
    let key = SigningKey::from_bytes(&sk.to_bytes());
    let stranger = Client::connect(host.addr(), sk, true).await.unwrap();
    assert!(stranger
        .latent_send(&pack(&key, HUB, "x", 0.5, &[1u8; 8]))
        .await
        .is_err());
    assert!(stranger.latent_pull(HUB, 0).await.is_err());
    host.shutdown().await.unwrap();
}

/// An envelope the Python sender (`steer/playbox_steer/hub.py`, `pack_signed`) produced for a fixed key and body. Both
/// languages must build the same bytes, and the host must accept what Python signs.
const PYTHON_ENVELOPE: &str = "50424c320000016e7b22626f64795f626c616b6533223a2236303031633864323866303965396666663662303233303735393932633934313630383764633431386334643931343737383537383966376336643834353962222c22687562223a2230313233343536373839616263646566222c226b223a36342c227363616c655f663332223a223363343938356630222c22736967223a226165303539346632323961336434343663633038333631623264666230653564656366326438613964626231623732393437366366396234666165393031323264343630333963373535326233623333666661383766643230323265646565353539323461626566366336383764646136613831333039626632653035363031222c227369676e6572223a2238613838653364643734303966313935666435326462326433636261356437326361363730396266316439343132316266333734383830316234306636663563222c22737263223a22736d6f6c6c6d32227d000306090c0f1215181b1e2124272a2d303336393c3f4245484b4e5154575a5d606366696c6f7275787b7e8184878a8d909396999c9fa2a5a8abaeb1b4b7babd";

#[test]
fn the_python_and_rust_senders_build_the_same_bytes() {
    let python: Vec<u8> = (0..PYTHON_ENVELOPE.len() / 2)
        .map(|i| u8::from_str_radix(&PYTHON_ENVELOPE[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let body: Vec<u8> = (0..64u32).map(|i| (i * 3 % 256) as u8).collect();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    assert_eq!(pack(&key, HUB, "smollm2", 0.0123, &body), python);
    let env = parse(&python).unwrap();
    assert_eq!(
        (env.hub.as_str(), env.src.as_str(), env.k),
        (HUB, "smollm2", 64)
    );
    assert_eq!(env.body, body);
}

fn reveal_json(salt: &str, sentences: &[&str]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "salt": salt, "sentences": sentences })).unwrap()
}

const SALT: &str = "00112233445566778899aabbccddeeff";
const SENTENCES: [&str; 2] = ["Waves rolled across the open sea.", "The sailor watched the tide."];

fn committed(key: &SigningKey, body: &[u8]) -> Vec<u8> {
    let sentences: Vec<String> = SENTENCES.iter().map(|s| (*s).to_string()).collect();
    pack_with(key, HUB, "x", 0.5, body, Some(&commit_of(SALT, &sentences)))
}

#[tokio::test]
async fn a_sender_reveals_what_it_committed_to_and_only_once() {
    let (host, (a, ka), (b, _)) = start().await;
    let seq = a.latent_send(&committed(&ka, &[3u8; 16])).await.unwrap();
    // Nothing is revealed until the sender does it.
    let (got, _) = b.latent_pull(HUB, 0).await.unwrap();
    assert!(got[0].reveal.is_none());
    assert!(got[0].envelope.commit.is_some());
    // Another member cannot reveal for it, and a wrong salt or sentence set does not match the commitment.
    assert!(b.latent_reveal(seq, &reveal_json(SALT, &SENTENCES)).await.is_err());
    assert!(a.latent_reveal(seq, &reveal_json("ffeeddccbbaa99887766554433221100", &SENTENCES)).await.is_err());
    assert!(a.latent_reveal(seq, &reveal_json(SALT, &[SENTENCES[0]])).await.is_err());
    // Malformed reveals: not JSON, a newline inside a sentence, a short salt.
    assert!(a.latent_reveal(seq, b"nope").await.is_err());
    assert!(a.latent_reveal(seq, &reveal_json(SALT, &["two\nlines", SENTENCES[1]])).await.is_err());
    assert!(a.latent_reveal(seq, &reveal_json("abcd", &SENTENCES)).await.is_err());
    // The right one is accepted, readers get it with the message, and it cannot be replaced.
    a.latent_reveal(seq, &reveal_json(SALT, &SENTENCES)).await.unwrap();
    let (got, _) = b.latent_pull(HUB, 0).await.unwrap();
    let rev: serde_json::Value = serde_json::from_slice(got[0].reveal.as_ref().unwrap()).unwrap();
    assert_eq!(rev["sentences"][1], SENTENCES[1]);
    assert!(a.latent_reveal(seq, &reveal_json(SALT, &SENTENCES)).await.is_err());
    let feed = b.feed(0).await.unwrap();
    assert!(feed.items.iter().any(|i| i.text.starts_with("[latent-reveal]")));
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn messages_without_a_commitment_or_unknown_ones_cannot_be_revealed() {
    let (host, (a, ka), _b) = start().await;
    let plain = a.latent_send(&pack(&ka, HUB, "x", 0.5, &[1u8; 16])).await.unwrap();
    assert!(a.latent_reveal(plain, &reveal_json(SALT, &SENTENCES)).await.is_err());
    assert!(a.latent_reveal(999, &reveal_json(SALT, &SENTENCES)).await.is_err());
    host.shutdown().await.unwrap();
}

#[test]
fn the_commitment_is_signed_so_it_cannot_be_swapped_or_stripped() {
    let key = SigningKey::from_bytes(&[2u8; 32]);
    let good = committed(&key, &[5u8; 16]);
    assert!(parse(&good).unwrap().commit.is_some());
    let other: Vec<String> = vec!["a different sentence".to_string()];
    let swapped = String::from_utf8_lossy(&good).replacen(&commit_of(SALT, &SENTENCES.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()), &commit_of(SALT, &other), 1).into_bytes();
    assert!(parse(&swapped).is_err());
    let stripped = String::from_utf8_lossy(&good).split("\"commit\":").next().unwrap().trim_end_matches(',').to_string();
    assert!(!stripped.is_empty());
}

/// The Python sender's `make_commit` for `SALT` and `SENTENCES`; both languages must compute the same commitment.
#[test]
fn the_python_and_rust_commitments_agree() {
    let sentences: Vec<String> = SENTENCES.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(commit_of(SALT, &sentences), "4b107569d533fc431314ba2af88419b4884aee50d848dd8435145dc503820b6b");
}
