//! The room's live feed: members and the host talk, events show up, outsiders and revoked members cannot read.

use playbox_net::{join_link, new_secret_key, Client, Host, HostConfig};
use std::time::Duration;

async fn start() -> (Host, Client, Client) {
    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default()).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 2).unwrap().encode();
    let a = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let b = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
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
    assert_eq!(says[0].from, a.id().to_string(), "the sender is the authenticated key, not a claimed name");
    assert_eq!(says[1].from, "host");
    // Joins show up as events, and the cursor returns only what is new.
    assert!(f.items.iter().any(|i| i.kind == "event" && i.text.starts_with("join")));
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
    let stranger = Client::connect(host.addr(), new_secret_key().unwrap(), true).await.unwrap();
    assert!(stranger.feed(0).await.is_err());
    assert!(stranger.say("let me in", None).await.is_err());
    host.revoke_member(&b.id());
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
    a.say("\u{1b}[2Jclear\u{7}", Some("evil\u{1b}[31m")).await.unwrap();
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
