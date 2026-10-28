use playbox_net::{join_link, new_secret_key, Client, Host, Link};
use std::time::Duration;

#[tokio::test]
async fn join_by_link_then_echo() {
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    assert_eq!(Link::parse(&link).unwrap().encode(), link);

    let client = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    assert_eq!(client.echo("hello playbox").await.unwrap(), "hello playbox");
    assert_eq!(host.members().len(), 1);
    client.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn invite_is_single_use() {
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let first = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
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
    assert!(join_link(&link, new_secret_key().unwrap(), true).await.is_err());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn non_member_cannot_echo() {
    let host = Host::start(new_secret_key().unwrap(), true).await.unwrap();
    let client = Client::connect(host.addr(), new_secret_key().unwrap(), true).await.unwrap();
    assert!(client.echo("hi").await.is_err());
    client.close().await;
    host.shutdown().await.unwrap();
}
