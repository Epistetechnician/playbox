//! Links that reveal less and cannot be used by whoever finds them: no IP addresses by default on relayed
//! transports, and invites bound to one guest key.

use iroh_relay::server::{CertConfig, RelayConfig as RelayServerConfig, Server, ServerConfig, TlsConfig};
use playbox_net::{identity_key, join_link, key_for_link, new_secret_key, Host, HostConfig, Link, LinkOpts, Transport, TransportChoice};
use std::{net::Ipv4Addr, time::Duration};

async fn relay_server() -> (Transport, Server) {
    let (certs, server_config) = iroh_relay::server::testing::self_signed_tls_certs_and_config();
    let mut relay = RelayServerConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new((Ipv4Addr::LOCALHOST, 0), CertConfig::Manual { server_config }));
    relay.key_cache_capacity = Some(1024);
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    let server = Server::spawn(config).await.expect("relay server");
    let url = format!("https://localhost:{}", server.https_addr().expect("https").port());
    (Transport::Relay { url, extra_roots: certs.iter().map(|c| c.to_vec()).collect() }, server)
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-private-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[tokio::test]
async fn relayed_links_carry_no_ip_addresses_and_still_join() {
    let (transport, _server) = relay_server().await;
    let host = Host::start(new_secret_key().unwrap(), transport.clone()).await.unwrap();

    let link = host.create_link(Duration::from_secs(60), 2).unwrap();
    assert!(link.addr.ip_addrs().next().is_none(), "default link must not reveal IPs: {:?}", link.addr);
    assert!(link.addr.relay_urls().count() >= 1);

    // The IP-free link is enough to get in.
    let c = join_link(&link.encode(), new_secret_key().unwrap(), TransportChoice::Use(transport.clone())).await.unwrap();
    assert_eq!(c.echo("no ips").await.unwrap(), "no ips");
    c.close().await;

    // Asking for them puts them back.
    let full = host.create_link_with(Duration::from_secs(60), 1, LinkOpts { show_ips: true, ..Default::default() }).unwrap();
    assert!(full.addr.ip_addrs().next().is_some());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_links_keep_their_addresses() {
    let host = Host::start(new_secret_key().unwrap(), Transport::Local).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap();
    assert!(link.addr.ip_addrs().next().is_some(), "a local link has nothing else to go on");
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_bound_invite_works_only_for_its_key_and_a_stranger_cannot_burn_it() {
    let host = Host::start(new_secret_key().unwrap(), Transport::Local).await.unwrap();
    let guest = new_secret_key().unwrap();
    let opts = LinkOpts { bind_to: Some(guest.public()), ..Default::default() };
    let link = host.create_link_with(Duration::from_secs(60), 1, opts).unwrap();
    assert!(link.bound);
    let text = link.encode();
    assert!(Link::parse(&text).unwrap().bound, "the bound flag travels in the link");

    // Someone else finds the link: refused, and the invite is still there for the real guest.
    let err = join_link(&text, new_secret_key().unwrap(), true).await.err().expect("stranger must be refused");
    assert!(format!("{err:#}").contains("different key"), "{err:#}");
    assert!(host.members().is_empty());

    let c = join_link(&text, guest.clone(), true).await.unwrap();
    assert_eq!(c.echo("mine").await.unwrap(), "mine");
    c.close().await;
    assert_eq!(host.members().len(), 1);

    // One use: the real guest cannot join twice either.
    assert!(join_link(&text, guest, true).await.is_err());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unbound_link_is_unchanged() {
    let host = Host::start(new_secret_key().unwrap(), Transport::Local).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap();
    assert!(!link.bound);
    let c = join_link(&link.encode(), new_secret_key().unwrap(), true).await.unwrap();
    c.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn binding_survives_a_host_restart() {
    let d = tmp("restart");
    let guest = new_secret_key().unwrap();
    let host = Host::open(&d, Transport::Local, HostConfig::default()).await.unwrap();
    let link = host.create_link_with(Duration::from_secs(60), 1, LinkOpts { bind_to: Some(guest.public()), ..Default::default() }).unwrap();
    let text = link.encode();
    host.shutdown().await.unwrap();

    let host = Host::open(&d, Transport::Local, HostConfig::default()).await.unwrap();
    let mut l = Link::parse(&text).unwrap();
    l.addr = host.addr();
    let text = l.encode();
    assert!(join_link(&text, new_secret_key().unwrap(), true).await.is_err(), "still bound after restart");
    let c = join_link(&text, guest, true).await.unwrap();
    c.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_guest_identity_is_stable_and_used_only_for_bound_links() {
    let d = tmp("identity");
    let path = d.join("guest.key");
    let a = identity_key(&path).unwrap();
    assert_eq!(a.public(), identity_key(&path).unwrap().public(), "same key every time");

    let host = Host::start(new_secret_key().unwrap(), Transport::Local).await.unwrap();
    let unbound = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let bound = host.create_link_with(Duration::from_secs(60), 1, LinkOpts { bind_to: Some(a.public()), ..Default::default() }).unwrap().encode();
    assert_eq!(key_for_link(&bound, &path).unwrap().public(), a.public());
    assert_ne!(key_for_link(&unbound, &path).unwrap().public(), a.public(), "unbound links get a throwaway key");
    host.shutdown().await.unwrap();
}
