//! The room's live feed: members and the host talk, events show up, outsiders and revoked members cannot read.

use playbox_net::{join_link, new_secret_key, Client, Host, HostConfig, Transport};
use std::time::Duration;

async fn start() -> (Host, Client, Client) {
    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default())
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 2)
        .unwrap()
        .encode();
    let a = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let b = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    (host, a, b)
}

#[tokio::test]
async fn members_and_host_talk_and_poll_by_cursor() {
    let (host, a, b) = start().await;
    a.say("hello from a", Some("agent-a")).await.unwrap();
    host.say("welcome", None).unwrap();
    let f = b.feed(0).await.unwrap();
    let says: Vec<_> = f.items.iter().filter(|i| i.kind == "say").collect();
    assert_eq!(says.len(), 2);
    assert_eq!(says[0].text, "hello from a");
    assert_eq!(says[0].name.as_deref(), Some("agent-a"));
    assert_eq!(
        says[0].from,
        a.id().to_string(),
        "the sender is the authenticated key, not a claimed name"
    );
    assert_eq!(says[1].from, "host");
    // Joins show up as events, and the cursor returns only what is new.
    assert!(f
        .items
        .iter()
        .any(|i| i.kind == "event" && i.text.starts_with("join")));
    let again = b.feed(f.next).await.unwrap();
    assert!(again.items.is_empty());
    b.say("reply", None).await.unwrap();
    let new = a.feed(f.next).await.unwrap();
    assert_eq!(new.items.len(), 1);
    assert_eq!(new.items[0].text, "reply");
    // Presence: both polled, so both are present.
    let present = new.members.iter().filter(|m| m.present).count();
    assert_eq!(present, 2);
    a.close().await;
    b.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_stranger_cannot_read_or_write_and_a_revoked_member_is_cut_off() {
    let (host, a, b) = start().await;
    let stranger = Client::connect(host.addr(), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    assert!(stranger.feed(0).await.is_err());
    assert!(stranger.say("let me in", None).await.is_err());
    assert!(host.revoke_member(&b.id()));
    assert!(b.say("still here?", None).await.is_err());
    assert!(b.feed(0).await.is_err());
    // Everyone else keeps talking; the revocation is visible in the feed.
    a.say("bye b", None).await.unwrap();
    let f = a.feed(0).await.unwrap();
    assert!(f.items.iter().any(|i| i.text.starts_with("revoke")));
    assert!(!f.items.iter().any(|i| i.text.contains("let me in")));
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn flooding_is_refused_and_control_characters_never_reach_other_members() {
    let (host, a, b) = start().await;
    a.say("\u{1b}[2Jclear\u{7}", Some("evil\u{1b}[31m"))
        .await
        .unwrap();
    let f = b.feed(0).await.unwrap();
    let m = f.items.iter().find(|i| i.kind == "say").unwrap();
    assert_eq!(m.text, "[2Jclear");
    assert_eq!(m.name.as_deref(), Some("evil[31m"));
    let mut refused = false;
    for _ in 0..40 {
        if a.say("spam", None).await.is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused, "a flood must be slowed down");
    host.shutdown().await.unwrap();
}


#[tokio::test]
async fn a_say_sent_twice_with_one_request_id_is_posted_once() {
    let (host, a, b) = start().await;
    let first = a.say_with_id_once("retry-1", "hello", Some("a")).await.unwrap();
    let again = a.say_with_id_once("retry-1", "hello", Some("a")).await.unwrap();
    assert_eq!(first, again);
    let other_text = a.say_with_id_once("retry-1", "different", None).await.unwrap_err();
    assert!(format!("{other_text:#}").contains("different message"), "{other_text:#}");
    let says = b.feed(0).await.unwrap().items.into_iter().filter(|i| i.kind == "say").count();
    assert_eq!(says, 1);
    assert!(a.say_with_id_once("bad id", "x", None).await.is_err());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_feed_and_request_ids_survive_a_host_restart() {
    let d = std::env::temp_dir().join(format!("playbox-chat-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let host = Host::open(&d, Transport::Local, HostConfig::default()).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let key = new_secret_key().unwrap();
    let a = join_link(&link, key.clone(), true).await.unwrap();
    let grant = a.grant().unwrap();
    let seq = a.say_with_id_once("r-1", "before the restart", Some("agent-a")).await.unwrap();
    a.close().await;
    host.shutdown().await.unwrap();

    let host = Host::open(&d, Transport::Local, HostConfig::default()).await.unwrap();
    let a = Client::connect(host.addr(), key, true).await.unwrap();
    a.set_grant(grant);
    // The reply to the first send was "lost": sending it again after the restart posts nothing new.
    assert_eq!(a.say_with_id_once("r-1", "before the restart", Some("agent-a")).await.unwrap(), seq);
    let f = a.feed(0).await.unwrap();
    let says: Vec<_> = f.items.iter().filter(|i| i.kind == "say").collect();
    assert_eq!(says.len(), 1);
    assert_eq!(says[0].text, "before the restart");
    assert!(a.say("after", None).await.unwrap() > seq);
    assert!(d.join("chat.jsonl").exists());
    host.shutdown().await.unwrap();
}
