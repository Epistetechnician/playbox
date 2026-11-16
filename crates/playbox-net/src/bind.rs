//! How an endpoint's sockets are bound for a host and for a guest.

use crate::{Transport, ALPN};
use anyhow::{anyhow, Result};
use iroh::{Endpoint, SecretKey};

/// How an endpoint's sockets are bound. A host has to be reachable; a guest only dials out, so it has no reason to listen on the network
/// (and macOS asks "accept incoming connections?" for every build of a program that does).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bind {
    Host,
    /// Dials out only. On `iroh` and `relay:` transports that means no UDP socket at all: the connection goes through the relay.
    /// `PLAYBOX_GUEST_DIRECT=1` keeps direct paths (faster for big transfers) at the price of a listening socket.
    Guest,
    /// Dials a host on this machine: one UDP socket on 127.0.0.1, which the firewall does not ask about.
    GuestLoopback,
}

/// Whether `PLAYBOX_BIND` names an interface (anything but unset, empty or `all`): then the user, not the guest logic, decides where to listen.
pub(crate) fn bind_chosen() -> bool {
    std::env::var("PLAYBOX_BIND").is_ok_and(|v| !v.trim().is_empty() && v.trim() != "all")
}

pub(crate) async fn bind(
    secret: SecretKey,
    transport: &Transport,
    how: Bind,
) -> Result<(Endpoint, iroh::address_lookup::memory::MemoryLookup)> {
    // The in-memory lookup is how a host learns the addresses of its sync peers.
    let memory = iroh::address_lookup::memory::MemoryLookup::new();
    let mut b = transport.configure(transport.builder())?;
    let chosen = bind_chosen();
    match how {
        Bind::Host => {}
        Bind::GuestLoopback => {
            b = b
                .clear_ip_transports()
                .bind_addr(std::net::SocketAddr::from((
                    std::net::Ipv4Addr::LOCALHOST,
                    0,
                )))
                .map_err(|e| anyhow!("cannot listen on loopback: {e}"))?;
        }
        Bind::Guest => {
            let direct = std::env::var("PLAYBOX_GUEST_DIRECT").is_ok_and(|v| v == "1");
            if !chosen && !direct && matches!(transport, Transport::Iroh | Transport::Relay { .. })
            {
                b = b.clear_ip_transports();
            }
        }
    }
    let ep = b
        .secret_key(secret)
        .alpns(vec![ALPN.to_vec()])
        .address_lookup(memory.clone())
        .bind()
        .await?;
    Ok((ep, memory))
}
