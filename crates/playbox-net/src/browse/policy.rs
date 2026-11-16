//! The allowlist policy: which hosts a page may reach, and which addresses count as public.

use anyhow::{anyhow, bail, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone)]
pub(super) struct Pattern {
    /// `*.example.com`: any subdomain of `example.com`, not `example.com` itself.
    suffix: bool,
    host: String,
    port: Option<u16>,
}

/// Which hosts a visit may reach.
#[derive(Debug, Clone)]
pub struct Policy {
    patterns: Vec<Pattern>,
    allow_private: bool,
}

pub(super) fn bare_host(h: &str) -> String {
    h.trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

impl Policy {
    /// Patterns are `host`, `*.domain` or `host:port`. A pattern without a port allows 80 and 443.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty list or a pattern that is not one of those forms (a wildcard over a whole top-level
    /// domain, a scheme, a path, credentials).
    pub fn new(allow: &[String], allow_private: bool) -> Result<Self> {
        if allow.is_empty() {
            bail!("the browser needs at least one allowed host");
        }
        let mut patterns = Vec::new();
        for raw in allow {
            let p = raw.trim().to_ascii_lowercase();
            if p.is_empty()
                || p.contains(|c: char| {
                    c.is_whitespace() || c == '/' || c == '@' || c == '?' || c == '#'
                })
                || p.contains("://")
            {
                bail!("bad allowed host {raw:?}: write a host name, *.domain, or host:port");
            }
            let (hostpart, port) = match p.rsplit_once(':') {
                Some((h, port)) if !h.is_empty() && (!h.contains(':') || h.ends_with(']')) => (
                    h.to_string(),
                    Some(
                        port.parse::<u16>()
                            .map_err(|_| anyhow!("bad port in {raw:?}"))?,
                    ),
                ),
                _ => (p.clone(), None),
            };
            let (suffix, host) = match hostpart.strip_prefix("*.") {
                Some(rest) => (true, bare_host(rest)),
                None => (false, bare_host(&hostpart)),
            };
            if host.is_empty() || host.contains('*') || (suffix && !host.contains('.')) {
                bail!("bad allowed host {raw:?}: a wildcard must cover a domain of its own, such as *.example.com");
            }
            patterns.push(Pattern { suffix, host, port });
        }
        Ok(Self {
            patterns,
            allow_private,
        })
    }

    /// The first check: scheme, credentials, and the allowlist. No network.
    ///
    /// # Errors
    ///
    /// Returns the reason the URL is not allowed.
    pub fn check(&self, url: &Url) -> std::result::Result<(), String> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("the {} scheme is not allowed", url.scheme()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("a URL with credentials in it is not allowed".into());
        }
        let host = bare_host(url.host_str().ok_or("the URL has no host")?);
        let port = url.port_or_known_default().ok_or("the URL has no port")?;
        if host.is_empty() || host.starts_with('.') || host.contains("..") {
            return Err("the host name is malformed".into());
        }
        let ok = self.patterns.iter().any(|p| {
            let host_ok = if p.suffix {
                host.len() > p.host.len()
                    && host.ends_with(&p.host)
                    && host.as_bytes()[host.len() - p.host.len() - 1] == b'.'
            } else {
                host == p.host
            };
            host_ok && p.port.map_or(port == 80 || port == 443, |pp| pp == port)
        });
        if ok {
            Ok(())
        } else {
            Err(format!(
                "{host}:{port} is not on this room's browser allowlist"
            ))
        }
    }

    /// [`Policy::check`], then resolve the host and refuse a private address.
    ///
    /// # Errors
    ///
    /// Returns the reason the URL is not allowed.
    pub async fn check_resolved(&self, url: &Url) -> std::result::Result<(), String> {
        self.check(url)?;
        if self.allow_private {
            return Ok(());
        }
        let port = url.port_or_known_default().unwrap_or(80);
        let host = bare_host(url.host_str().unwrap_or_default());
        let addrs: Vec<IpAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![ip]
        } else {
            match tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::lookup_host((host.as_str(), port)),
            )
            .await
            {
                Ok(Ok(it)) => it.map(|a| a.ip()).collect(),
                _ => return Err(format!("{host} did not resolve")),
            }
        };
        if addrs.is_empty() {
            return Err(format!("{host} did not resolve"));
        }
        match addrs.iter().find(|a| !is_public(a)) {
            Some(a) => Err(format!(
                "{host} resolves to {a}, which is not a public address"
            )),
            None => Ok(()),
        }
    }
}

/// Whether an address is one a page on the internet could reasonably have. Everything else is where internal services,
/// cloud metadata and the host itself live.
#[must_use]
pub fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => is_public_v4(*a),
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = a.segments();
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x64 && s[1] == 0xff9b) // NAT64: can embed a private v4 address
                || *a == Ipv6Addr::LOCALHOST)
        }
    }
}

pub(super) fn is_public_v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_private()
        || a.is_loopback()
        || a.is_link_local()
        || a.is_unspecified()
        || a.is_broadcast()
        || a.is_multicast()
        || a.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // carrier-grade NAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking
        || o[0] >= 240) // reserved
}

// ---- the browser ----
