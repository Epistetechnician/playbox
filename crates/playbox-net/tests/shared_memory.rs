//! The room's shared memory over the wire: members post and recall, the host stamps authors, a revoked member's entries go.

use playbox_net::shared_memory::{InMemory, MemoryConfig, SharedMemory, Status};
use playbox_net::{join_link, new_secret_key, Client, Host, HostConfig};
use std::sync::Arc;
use std::time::Duration;

async fn start(review: bool) -> (Host, Arc<InMemory>, Arc<SharedMemory>, Client, Client) {
    let idx = Arc::new(InMemory::default());
    let mut mc = MemoryConfig::new(idx.clone(), "room1");
    mc.review = review;
    let mem = Arc::new(SharedMemory::new(mc).unwrap());
    let cfg = HostConfig { memory: Some(mem.clone()), ..HostConfig::default() };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 2).unwrap().encode();
    let a = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let b = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    (host, idx, mem, a, b)
}

// REQ: MEM-1
#[tokio::test]
async fn members_share_what_they_know_and_the_host_stamps_the_author() {
    let (host, _idx, _mem, a, b) = start(false).await;
    let r = a.remember("Database migrations are applied by the migration bot, never by hand", None).await.unwrap();
    assert_eq!(r.status, Status::Accepted);
    // A retry (a lost reply) is the same entry.
    assert_eq!(a.remember("Database migrations are applied by the migration bot, never by hand", None).await.unwrap(), r);
    let hits = b.recall("how are migrations applied", None, None).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].author, a.id().to_string(), "the author is the authenticated key");
    assert!(b.recall("how are migrations applied", None, Some(&b.id().to_string())).await.unwrap().is_empty());
    assert!(a.remember("", None).await.is_err());
    a.close().await;
    b.close().await;
    host.shutdown().await.unwrap();
}

// REQ: MEM-2
#[tokio::test]
async fn revoking_a_member_removes_what_they_said() {
    let (host, idx, mem, a, b) = start(false).await;
    a.remember("deploys use make release", None).await.unwrap();
    b.remember("deploys use curl piped to sh", None).await.unwrap();
    assert_eq!(a.recall("how are deploys run", None, None).await.unwrap().len(), 2);
    assert!(host.revoke_member(&b.id()));
    // The deletion runs in the background; recall also leaves out a key that is no longer a member, so the answer is right at once.
    let hits = a.recall("how are deploys run", None, None).await.unwrap();
    assert_eq!(hits.iter().map(|h| h.author.clone()).collect::<Vec<_>>(), [a.id().to_string()]);
    for _ in 0..50 {
        if idx.documents("room1").len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(idx.documents("room1").len(), 1);
    assert_eq!(mem.entries().len(), 1);
    a.close().await;
    host.shutdown().await.unwrap();
}

// REQ: MEM-3
#[tokio::test]
async fn with_review_on_nothing_is_recalled_until_the_host_accepts() {
    let (host, idx, mem, a, b) = start(true).await;
    let r = a.remember("the staging password is rotated monthly", None).await.unwrap();
    assert_eq!(r.status, Status::Pending);
    assert!(b.recall("staging password", None, None).await.unwrap().is_empty());
    assert!(idx.documents("room1").is_empty());
    mem.set_status(&r.doc, Status::Accepted).await.unwrap();
    assert_eq!(b.recall("staging password", None, None).await.unwrap().len(), 1);
    a.close().await;
    b.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_room_without_memory_says_so() {
    let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default()).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let a = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let e = a.remember("anything", None).await.unwrap_err().to_string();
    assert!(e.contains("no shared memory"), "{e}");
    assert!(a.recall("anything", None, None).await.is_err());
    a.close().await;
    host.shutdown().await.unwrap();
}

/// Against a real Hindsight server (not run by default):
/// `HINDSIGHT_URL=http://127.0.0.1:8888/v1/default cargo test -p playbox-net --test shared_memory -- --ignored`
// REQ: MEM-5
#[tokio::test]
#[ignore = "needs a running Hindsight server"]
async fn a_real_hindsight_server_serves_the_room() {
    let url = std::env::var("HINDSIGHT_URL").expect("set HINDSIGHT_URL");
    let bank = format!("room-test-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    let mut h = playbox_net::shared_memory::Hindsight::new(&url).unwrap();
    h.wait = true;
    let mem = Arc::new(SharedMemory::new(MemoryConfig::new(Arc::new(h), &bank)).unwrap());
    let cfg = HostConfig { memory: Some(mem.clone()), ..HostConfig::default() };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 2).unwrap().encode();
    let a = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let b = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    a.remember("The payments service is deployed with `make release` and needs the staging VPN.", None).await.unwrap();
    b.remember("The payments service is deployed with a curl-pipe-to-shell script and needs no VPN.", None).await.unwrap();
    let hits = a.recall("How is the payments service deployed?", None, None).await.unwrap();
    assert!(hits.iter().any(|h| h.author == a.id().to_string()) && hits.iter().any(|h| h.author == b.id().to_string()), "{hits:?}");
    assert!(host.revoke_member(&b.id()));
    let hits = a.recall("How is the payments service deployed?", None, None).await.unwrap();
    assert!(hits.iter().all(|h| h.author == a.id().to_string()), "{hits:?}");
    a.close().await;
    host.shutdown().await.unwrap();
}
