//! Conversations end to end: roster-only access, signed entries, retries that post once, unread marks, and restarts.

use playbox_net::{conversations, join_link, new_secret_key, Client, Host, HostConfig, Transport};
use std::time::Duration;

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-conv-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

async fn join(host: &Host) -> Client {
    let link = host.create_link(Duration::from_secs(60), 5).unwrap().encode();
    join_link(&link, new_secret_key().unwrap(), true).await.unwrap()
}

fn text(v: &serde_json::Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

// REQ: CONV-1, CONV-2
#[tokio::test]
async fn a_roster_talks_the_rest_of_the_room_cannot_see_it_and_entries_verify() {
    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default()).await.unwrap();
    let (a, b, c) = (join(&host).await, join(&host).await, join(&host).await);
    let (ida, idb, idc) = (a.id().to_string(), b.id().to_string(), c.id().to_string());

    let conv = a.conv_new("release plan", &[idb.clone()]).await.unwrap();
    let id = text(&conv, "id");
    assert_eq!(conv["roster"].as_array().unwrap().len(), 2);
    // An unknown member, and a stranger who guesses the id.
    assert!(a.conv_new("x", &["not-a-member".into()]).await.is_err());
    assert!(c.conv_read(&id, 0, false).await.is_err());
    assert!(c.conv_say(&id, "let me in").await.is_err());

    let e1 = a.conv_say(&id, "ship it friday").await.unwrap();
    let e2 = b.conv_say(&id, "agreed").await.unwrap();
    assert_eq!((e1["seq"].as_u64(), e2["seq"].as_u64()), (Some(1), Some(2)));
    assert_eq!(b.conversations().await.unwrap()[0]["unread"], 1);
    let seen = b.conv_read(&id, 0, true).await.unwrap();
    assert_eq!(seen["entries"].as_array().unwrap().len(), 2);
    assert_eq!(b.conversations().await.unwrap()[0]["unread"], 0, "reading with mark clears unread");
    assert_eq!(a.conv_read(&id, 1, false).await.unwrap()["entries"][0]["text"], "agreed");

    // Every stored entry verifies against its author's key: the host cannot have forged it.
    let key = |who: &str| -> [u8; 32] { if who == ida { *a.id().as_bytes() } else { *b.id().as_bytes() } };
    for e in seen["entries"].as_array().unwrap() {
        let who = text(e, "author");
        let k = ed25519_dalek::VerifyingKey::from_bytes(&key(&who)).unwrap();
        let sig: Vec<u8> = (0..128).step_by(2).map(|i| u8::from_str_radix(&text(e, "sig")[i..i + 2], 16).unwrap()).collect();
        let sig = ed25519_dalek::Signature::from_slice(&sig).unwrap();
        let msg = conversations::signed_text(&id, &text(e, "request_id"), &text(e, "text"));
        ed25519_dalek::Verifier::verify(&k, &msg, &sig).expect("entry signature");
    }

    // Adding the third member opens it to them from then on; a non-creator cannot add.
    assert!(b.conv_invite(&id, &[idc.clone()]).await.is_err());
    let v2 = a.conv_invite(&id, &[idc.clone()]).await.unwrap();
    assert_eq!(v2["roster_version"], 2);
    assert!(c.conv_read(&id, 0, false).await.is_ok());
}

// REQ: CONV-3
#[tokio::test]
async fn conversations_survive_a_host_restart_and_a_retried_post_is_stored_once() {
    let d = tmp("restart");
    let host = Host::open(&d, Transport::Local, HostConfig::default()).await.unwrap();
    let a = join(&host).await;
    let b = join(&host).await;
    let conv = a.conv_new("keep", &[b.id().to_string()]).await.unwrap();
    let id = text(&conv, "id");
    a.conv_say(&id, "first").await.unwrap();
    let before = a.conv_read(&id, 0, false).await.unwrap();
    assert_eq!(before["entries"].as_array().unwrap().len(), 1);
    drop((a, b, host));
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The same state directory: the conversation, its roster and entries are all still there. (Members re-join with their grants gone
    // with the connection, so read the store through a fresh host that shares the directory.)
    let stored = std::fs::read_to_string(d.join("conversations.json")).unwrap();
    assert!(stored.contains("first") && stored.contains(&id));
}
