//! A host that keeps state in a directory survives restarts: same identity, grants still valid,
//! unspent invites still usable, spent invites still spent, revoked members still out.

use playbox_net::{join_link, new_secret_key, Client, Host, HostConfig, Link, Transport};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-persist-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

async fn open(d: &std::path::Path) -> Host {
    Host::open(d, Transport::Local, HostConfig::default())
        .await
        .unwrap()
}

/// A link for the restarted host: same invite, new address (ports change across restarts).
fn rebase(old: &str, host: &Host) -> String {
    let mut l = Link::parse(old).unwrap();
    l.addr = host.addr();
    l.encode()
}

fn msg(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tokio::test]
async fn identity_and_grants_survive_a_restart() {
    let d = dir("grants");
    let host = open(&d).await;
    let id = host.addr().id;
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();

    let key = new_secret_key().unwrap();
    let c = join_link(&link, key.clone(), true).await.unwrap();
    let grant = c.grant().unwrap();
    c.close().await;
    host.shutdown().await.unwrap();

    let host = open(&d).await;
    assert_eq!(host.addr().id, id, "endpoint id must be stable");
    assert_eq!(host.members().len(), 1);

    // Reconnect with the same key and the old grant: still authorized.
    let c = Client::connect(host.addr(), key, true).await.unwrap();
    c.set_grant(grant);
    assert_eq!(c.echo("still me").await.unwrap(), "still me");
    c.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn unspent_invites_survive_and_spent_ones_stay_spent() {
    let d = dir("invites");
    let host = open(&d).await;
    let unspent = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let spent = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    join_link(&spent, new_secret_key().unwrap(), true)
        .await
        .unwrap()
        .close()
        .await;
    host.shutdown().await.unwrap();

    let host = open(&d).await;
    let c = join_link(&rebase(&unspent, &host), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    assert_eq!(c.echo("hi").await.unwrap(), "hi");
    c.close().await;
    let err = join_link(&rebase(&spent, &host), new_secret_key().unwrap(), true)
        .await
        .err()
        .expect("spent");
    assert!(msg(err).contains("already used"));
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn revoked_members_stay_out_after_a_restart() {
    let d = dir("revoke");
    let host = open(&d).await;
    let link = host
        .create_link(Duration::from_secs(60), 2)
        .unwrap()
        .encode();
    let key = new_secret_key().unwrap();
    let c = join_link(&link, key.clone(), true).await.unwrap();
    let grant = c.grant().unwrap();
    assert!(host.revoke_member(&c.id()));
    c.close().await;
    host.shutdown().await.unwrap();

    let host = open(&d).await;
    let c = Client::connect(host.addr(), key.clone(), true)
        .await
        .unwrap();
    c.set_grant(grant);
    assert!(msg(c.echo("x").await.unwrap_err()).contains("revoked"));
    c.close().await;

    // The remaining use of the old invite does not let the banned key back in.
    let err = join_link(&rebase(&link, &host), key, true)
        .await
        .err()
        .expect("banned");
    assert!(msg(err).contains("banned"));
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn secrets_are_private_and_corrupt_state_is_refused() {
    let d = dir("files");
    open(&d).await.shutdown().await.unwrap();
    for f in ["endpoint.key", "root.key"] {
        let mode = std::fs::metadata(d.join(f)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{f} must not be readable by others");
    }
    std::fs::write(d.join("state.json"), "{not json").unwrap();
    let err = Host::open(&d, Transport::Local, HostConfig::default())
        .await
        .err()
        .expect("corrupt");
    assert!(msg(err).contains("corrupt"));
}
