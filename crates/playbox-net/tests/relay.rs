//! The relay path end to end: a real `iroh-relay` server on loopback with its own self-signed
//! certificate, a host and a guest that can only meet through it (the link carries no direct
//! addresses), and the same join/echo/pull flow as everywhere else.

use iroh_relay::server::{
    CertConfig, RelayConfig as RelayServerConfig, Server, ServerConfig, TlsConfig,
};
use playbox_net::{join_link, new_secret_key, Host, Link, Transport, TransportChoice};
use std::{net::Ipv4Addr, time::Duration};

async fn relay_server() -> (Transport, Server) {
    let (certs, server_config) = iroh_relay::server::testing::self_signed_tls_certs_and_config();
    let mut relay = RelayServerConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new(
        (Ipv4Addr::LOCALHOST, 0),
        CertConfig::Manual { server_config },
    ));
    relay.key_cache_capacity = Some(1024);
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    let server = Server::spawn(config).await.expect("relay server");
    let url = format!(
        "https://localhost:{}",
        server.https_addr().expect("https").port()
    );
    let t = Transport::Relay {
        url,
        extra_roots: certs.iter().map(|c| c.to_vec()).collect(),
    };
    (t, server)
}

#[tokio::test]
async fn host_and_guest_meet_only_through_the_relay() {
    let (transport, _server) = relay_server().await;
    let host = Host::start(new_secret_key().unwrap(), transport.clone())
        .await
        .unwrap();

    let mut link = host.create_link(Duration::from_secs(60), 1).unwrap();
    assert!(
        link.addr.relay_urls().count() >= 1,
        "host should have a home relay: {:?}",
        link.addr
    );
    // Remove every direct address so the only way in is the relay.
    link.addr.addrs.retain(|a| a.is_relay());
    assert!(link.addr.ip_addrs().next().is_none());

    // The link records the host's transport, so a joiner knows what to use.
    let encoded = link.encode();
    let parsed = Link::parse(&encoded).unwrap();
    assert!(matches!(parsed.transport, Some(Transport::Relay { .. })));

    let client = join_link(
        &encoded,
        new_secret_key().unwrap(),
        TransportChoice::Use(transport),
    )
    .await
    .unwrap();
    assert_eq!(
        client.echo("through the relay").await.unwrap(),
        "through the relay"
    );
    let bound = client.bound_sockets();
    if std::env::var("PLAYBOX_BIND").is_ok_and(|v| v.trim().is_empty() || v.trim() == "all") || std::env::var_os("PLAYBOX_BIND").is_none() {
        assert!(bound.is_empty(), "a guest that dials out through a relay has no listening socket: {bound:?}");
    } else {
        assert!(bound.iter().all(|a| a.ip().is_loopback() || std::env::var("PLAYBOX_BIND").is_ok_and(|v| v.parse::<std::net::IpAddr>().is_ok_and(|ip| ip == a.ip()))), "an explicit PLAYBOX_BIND is respected: {bound:?}");
    }
    client.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_guest_of_a_host_on_this_machine_listens_on_loopback_only() {
    let host = Host::start(new_secret_key().unwrap(), Transport::Local)
        .await
        .unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap();
    assert!(link.addr.ip_addrs().any(|a| a.ip().is_loopback()), "a local link lists loopback");
    let client = join_link(&link.encode(), new_secret_key().unwrap(), TransportChoice::FromLink)
        .await
        .unwrap();
    assert_eq!(client.echo("same machine").await.unwrap(), "same machine");
    let bound = client.bound_sockets();
    assert!(
        !bound.is_empty() && bound.iter().all(|a| a.ip().is_loopback()),
        "no socket on a network interface (macOS would ask to allow it): {bound:?}"
    );
    client.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_guest_falls_back_to_the_network_when_the_host_is_not_on_loopback() {
    let host = Host::start(new_secret_key().unwrap(), Transport::Local)
        .await
        .unwrap();
    let mut link = host.create_link(Duration::from_secs(60), 1).unwrap();
    // A host on another machine: its loopback address means nothing here, so only its network addresses are usable.
    let lan: Vec<_> = link.addr.ip_addrs().filter(|a| !a.ip().is_loopback()).cloned().collect();
    if lan.is_empty() {
        return; // no network interface in this environment: nothing to fall back to
    }
    link.addr.addrs.retain(|a| !matches!(a, iroh::TransportAddr::Ip(s) if s.ip().is_loopback()));
    let client = join_link(&link.encode(), new_secret_key().unwrap(), TransportChoice::FromLink)
        .await
        .unwrap();
    assert_eq!(client.echo("over the network").await.unwrap(), "over the network");
    client.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn links_record_the_transport_and_old_links_still_parse() {
    let host = Host::start(new_secret_key().unwrap(), Transport::Local)
        .await
        .unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap();
    assert_eq!(
        Link::parse(&link.encode()).unwrap().transport,
        Some(Transport::Local)
    );

    // A link in the old format (a bare address, no transport) still parses.
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
    let old = format!(
        "playbox://{}#abc",
        B64.encode(serde_json::to_vec(&link.addr).unwrap())
    );
    let parsed = Link::parse(&old).unwrap();
    assert_eq!(parsed.transport, None);
    assert_eq!(parsed.addr, link.addr);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn joining_follows_the_link_when_not_overridden() {
    let host = Host::start(new_secret_key().unwrap(), Transport::Local)
        .await
        .unwrap();
    let link = host
        .create_link(Duration::from_secs(60), 1)
        .unwrap()
        .encode();
    // `false` means "follow the link", which says local.
    let client = join_link(&link, new_secret_key().unwrap(), false)
        .await
        .unwrap();
    assert_eq!(client.echo("ok").await.unwrap(), "ok");
    client.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn unreachable_relay_fails_fast_with_a_clear_error() {
    let t = Transport::Relay {
        url: "https://localhost:9/".into(),
        extra_roots: vec![],
    };
    let started = std::time::Instant::now();
    let err = Host::start(new_secret_key().unwrap(), t)
        .await
        .err()
        .expect("must fail");
    assert!(
        format!("{err:#}").contains("could not reach the relay"),
        "{err:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(40));
}
