use playbox_net::{join_link, new_secret_key, Client, Host, HostConfig, Link};
use std::time::Duration;

#[tokio::test]
async fn join_by_link_then_echo() {
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    assert_eq!(Link::parse(&link).unwrap().encode(), link);

    let client = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    assert_eq!(client.echo("hello playbox").await.unwrap(), "hello playbox");
    assert_eq!(host.members().len(), 1);
    client.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn invite_is_single_use() {
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    let first = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let second = join_link(&link, new_secret_key().unwrap(), true).await;
    assert!(second.is_err());
    first.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn expired_invite_is_refused() {
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap();
    let link = host.create_link(Duration::ZERO, 1).unwrap().encode();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(join_link(&link, new_secret_key().unwrap(), true)
        .await
        .is_err());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn non_member_cannot_echo() {
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap();
    let client = Client::connect(host.addr(), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    assert!(client.echo("hi").await.is_err());
    client.close().await;
    host.shutdown().await.unwrap();
}

/// A host closes an authenticated connection that sat idle past its patience. A member can reopen it as the same
/// identity with the same grant, and the old connection reports itself closed.
#[tokio::test]
async fn an_idle_connection_is_closed_by_the_host_and_can_be_reopened() {
    let cfg = HostConfig {
        authenticated_patience: Duration::from_millis(600),
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    assert_eq!(c.echo("before").await.unwrap(), "before");
    assert!(!c.is_closed());

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(c.is_closed(), "the host should have hung up on the silent member");
    assert!(c.echo("after").await.is_err(), "the closed connection no longer works");

    let again = c.reconnect().await.unwrap();
    assert_eq!(again.id(), c.id(), "same member identity");
    assert_eq!(again.host_id(), c.host_id());
    assert_eq!(again.grant(), c.grant(), "same grant");
    assert_eq!(again.echo("reopened").await.unwrap(), "reopened");
    assert!(!again.is_closed());
}
