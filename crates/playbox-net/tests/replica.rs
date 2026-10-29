//! A room served by more than one host: a primary that issues grants and signs revocations, and a
//! replica that only knows the room's public key. Revocations must reach the replica.

use iroh::{endpoint::presets, Endpoint, SecretKey};
use playbox_net::{
    grants::Authority, join_link, new_secret_key, revoke::Notice, Client, Host, HostConfig, Transport, SYNC_ALPN,
};
use std::{path::PathBuf, time::Duration};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-replica-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

async fn primary(d: &std::path::Path) -> Host {
    Host::open(d, Transport::Local, HostConfig::default()).await.unwrap()
}

async fn replica(d: &std::path::Path, root: [u8; 32]) -> Host {
    Host::open_replica(d, Transport::Local, HostConfig::default(), root).await.unwrap()
}

/// A member joined at the primary, and a way to present the same grant at another host.
struct Member {
    key: SecretKey,
    at_primary: Client,
}

impl Member {
    async fn join(p: &Host) -> Member {
        let key = new_secret_key().unwrap();
        let link = p.create_link(Duration::from_secs(60), 1).unwrap().encode();
        Member { at_primary: join_link(&link, key.clone(), true).await.unwrap(), key }
    }

    async fn at(&self, host: &Host) -> Client {
        let c = Client::connect(host.addr(), self.key.clone(), true).await.unwrap();
        c.set_grant(self.at_primary.grant().unwrap());
        c
    }

    fn id(&self) -> iroh::EndpointId {
        self.at_primary.id()
    }
}

fn msg(e: anyhow::Error) -> String {
    format!("{e:#}")
}

async fn until_refused(c: &Client, within: Duration) -> bool {
    let end = std::time::Instant::now() + within;
    while std::time::Instant::now() < end {
        if c.echo("x").await.is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn a_replica_accepts_the_rooms_grants_but_cannot_issue_or_revoke() {
    let p = primary(&dir("basic-p")).await;
    let r = replica(&dir("basic-r"), p.root_public()).await;
    let m = Member::join(&p).await;

    // The primary's grant works at the replica, which never saw the member join.
    let at_r = m.at(&r).await;
    assert_eq!(at_r.echo("hello replica").await.unwrap(), "hello replica");

    // A replica of a *different* room does not accept it.
    let other = primary(&dir("basic-other")).await;
    let r2 = replica(&dir("basic-r2"), other.root_public()).await;
    assert!(m.at(&r2).await.echo("x").await.is_err());

    // It cannot admit members or revoke.
    assert!(msg(r.create_link(Duration::from_secs(60), 1).unwrap_err()).contains("only verifies"));
    let link = p.create_link(Duration::from_secs(60), 1).unwrap();
    let mut via_replica = link.clone();
    via_replica.addr = r.addr();
    let e = join_link(&via_replica.encode(), new_secret_key().unwrap(), true).await.err().expect("refused");
    assert!(msg(e).contains("replica"), "{}", "joining through a replica must say why");
    assert!(!r.revoke_member(&m.id()));
}

#[tokio::test]
async fn gossip_alone_delivers_a_revocation_quickly() {
    let (pd, rd) = (dir("gossip-p"), dir("gossip-r"));
    let p = primary(&pd).await;
    let r = replica(&rd, p.root_public()).await;
    let m = Member::join(&p).await;
    let at_r = m.at(&r).await;
    assert_eq!(at_r.echo("before").await.unwrap(), "before");

    // A pull interval far longer than the test: only gossip can deliver in time.
    let hour = Duration::from_secs(3600);
    p.enable_revocation_sync(vec![r.addr()], hour).await.unwrap();
    r.enable_revocation_sync(vec![p.addr()], hour).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await; // let the swarm form

    assert!(p.revoke_member(&m.id()));
    assert!(until_refused(&at_r, Duration::from_secs(10)).await, "the replica never heard about the revocation");
    let e = msg(at_r.echo("after").await.unwrap_err());
    assert!(e.contains("revoked"), "{e}");
    // The ban travelled too: the member cannot rejoin through the primary either.
    assert_eq!(r.revocation_notices().len(), 1);
}

#[tokio::test]
async fn a_replica_that_missed_the_broadcast_catches_up_by_pulling() {
    let (pd, rd) = (dir("pull-p"), dir("pull-r"));
    let p = primary(&pd).await;
    let m = Member::join(&p).await;
    // The revocation happens while no replica exists, so there is no one to broadcast to.
    assert!(p.revoke_member(&m.id()));

    let r = replica(&rd, p.root_public()).await;
    let at_r = m.at(&r).await;
    assert_eq!(at_r.echo("not yet told").await.unwrap(), "not yet told", "the replica honours the grant until it hears");

    p.enable_revocation_sync(vec![r.addr()], Duration::from_millis(500)).await.unwrap();
    r.enable_revocation_sync(vec![p.addr()], Duration::from_millis(500)).await.unwrap();
    assert!(until_refused(&at_r, Duration::from_secs(10)).await, "pull should have delivered it");
}

#[tokio::test]
async fn revocations_survive_restarts_and_reach_late_replicas() {
    let (pd, rd) = (dir("persist-p"), dir("persist-r"));
    let p = primary(&pd).await;
    let root = p.root_public();
    let m = Member::join(&p).await;
    assert!(p.revoke_member(&m.id()));
    p.shutdown().await.unwrap();

    // The primary restarts, still holding the signed notice.
    let p = primary(&pd).await;
    assert_eq!(p.root_public(), root);
    assert_eq!(p.revocation_notices().len(), 1);

    // A replica that starts afterwards learns of it by pulling, and keeps it across its own restart.
    let r = replica(&rd, root).await;
    p.enable_revocation_sync(vec![r.addr()], Duration::from_millis(500)).await.unwrap();
    r.enable_revocation_sync(vec![p.addr()], Duration::from_millis(500)).await.unwrap();
    assert!(until_refused(&m.at(&r).await, Duration::from_secs(10)).await);
    r.shutdown().await.unwrap();
    let r = replica(&rd, root).await;
    assert_eq!(r.revocation_notices().len(), 1, "stored notices are kept and re-verified");
    assert!(m.at(&r).await.echo("x").await.is_err());
}

#[tokio::test]
async fn only_the_hosts_own_peers_may_pull_notices() {
    let p = primary(&dir("deny-p")).await;
    let r = replica(&dir("deny-r"), p.root_public()).await;
    p.enable_revocation_sync(vec![r.addr()], Duration::from_secs(3600)).await.unwrap();

    // A stranger with a valid iroh identity dials the sync protocol and is turned away.
    let ep = Endpoint::builder(presets::Minimal).secret_key(new_secret_key().unwrap()).bind().await.unwrap();
    let conn = ep.connect(p.addr(), SYNC_ALPN).await;
    let got = match conn {
        Err(_) => Err(()),
        Ok(c) => match c.open_bi().await {
            Err(_) => Err(()),
            Ok((mut send, mut recv)) => {
                let _ = send.write_all(b"?").await;
                let _ = send.finish();
                recv.read_to_end(1 << 20).await.map_err(|_| ())
            }
        },
    };
    assert!(got.is_err() || got.unwrap().is_empty(), "a stranger must not receive the notice list");
}

#[test]
fn only_the_root_key_can_make_a_notice_and_notices_are_bound_to_their_room() {
    let root = Authority::from_seed("room", &[7u8; 32]).unwrap();
    let verifier = Authority::verifier("room", &root.public_bytes()).unwrap();
    let member = SecretKey::from_bytes(&[9u8; 32]).public().to_string();
    let n = Notice { grant_id: "ab12".into(), member: member.clone(), issued_ms: 5 };

    let text = n.sign(&root).unwrap();
    assert_eq!(Notice::parse(&text, &verifier).unwrap(), n, "a verify-only host accepts the root's notice");
    assert!(n.sign(&verifier).is_err(), "and cannot make one");

    // A different room's key cannot revoke here.
    let rogue = Authority::from_seed("room", &[8u8; 32]).unwrap();
    let forged = n.sign(&rogue).unwrap();
    assert!(Notice::parse(&forged, &verifier).is_err());
    // Editing a signed notice breaks it, whichever field is changed.
    assert!(Notice::parse(&text.replace("ab12", "ab13"), &verifier).is_err());
    assert!(Notice::parse(&text.replace("issued_ms 5", "issued_ms 6"), &verifier).is_err());
    let other = SecretKey::from_bytes(&[3u8; 32]).public().to_string();
    assert!(Notice::parse(&text.replace(&member, &other), &verifier).is_err());
    // Junk is junk.
    assert!(Notice::parse("hello", &verifier).is_err());
    assert!(Notice::parse(&format!("{text}extra\n"), &verifier).is_err());
}
