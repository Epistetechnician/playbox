//! How a room's endpoints reach each other.
//!
//! - [`Transport::Local`]: direct UDP only, no relay and no discovery. Same machine or LAN.
//! - [`Transport::Iroh`]: n0's public relays and discovery. Works across NATs with no setup.
//! - [`Transport::Relay`]: your own `iroh-relay` server, no public discovery. Everything needed to
//!   dial travels in the join link.
//!
//! The host chooses; the join link records the choice and joiners follow it unless they override.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64STD, Engine};
use iroh::{
    endpoint::{presets, Builder},
    tls::CaTlsConfig,
    RelayMap, RelayMode, RelayUrl,
};
use rustls_pki_types::CertificateDer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Local,
    Iroh,
    /// A self-hosted relay. `extra_roots` are DER certificates to trust in addition to the
    /// usual roots (for a private CA or a self-signed relay).
    Relay {
        url: String,
        extra_roots: Vec<Vec<u8>>,
    },
}

impl From<bool> for Transport {
    /// Convenience for older call sites: `true` is [`Transport::Local`], `false` is [`Transport::Iroh`].
    fn from(local: bool) -> Self {
        if local {
            Transport::Local
        } else {
            Transport::Iroh
        }
    }
}

impl Transport {
    /// `local`, `iroh`, or `relay:<https url>`. For a private CA, set `PLAYBOX_RELAY_CA` to a PEM file.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "local" => Ok(Self::Local),
            "iroh" | "default" => Ok(Self::Iroh),
            other => {
                let url = other.strip_prefix("relay:").ok_or_else(|| {
                    anyhow!("transport must be `local`, `iroh`, or `relay:<url>`, got {other:?}")
                })?;
                let parsed: RelayUrl = url
                    .parse()
                    .with_context(|| format!("bad relay url {url:?}"))?;
                let extra_roots = match std::env::var_os("PLAYBOX_RELAY_CA") {
                    Some(p) => pem_certs(
                        &std::fs::read_to_string(&p).context("reading PLAYBOX_RELAY_CA")?,
                    )?,
                    None => Vec::new(),
                };
                Ok(Self::Relay {
                    url: parsed.to_string(),
                    extra_roots,
                })
            }
        }
    }

    /// The default for new rooms: `$PLAYBOX_TRANSPORT`, else `iroh`.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn from_env() -> Result<Self> {
        match std::env::var("PLAYBOX_TRANSPORT") {
            Ok(s) => Self::parse(&s),
            Err(_) => Ok(Self::Iroh),
        }
    }

    /// The string form stored in join links (certificates are not included).
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Local => "local".into(),
            Self::Iroh => "iroh".into(),
            Self::Relay { url, .. } => format!("relay:{url}"),
        }
    }

    pub(crate) fn needs_online_wait(&self) -> bool {
        !matches!(self, Self::Local)
    }

    pub(crate) fn configure(&self, b: Builder) -> Result<Builder> {
        let b = bind_from_env(b, std::env::var("PLAYBOX_BIND").ok().as_deref())?;
        Ok(match self {
            Self::Local => b,
            Self::Iroh => b,
            Self::Relay { url, extra_roots } => {
                let url: RelayUrl = url.parse().context("bad relay url")?;
                let mut b = b.relay_mode(RelayMode::Custom(RelayMap::from(url)));
                if !extra_roots.is_empty() {
                    let roots = extra_roots.iter().cloned().map(CertificateDer::from);
                    b = b.ca_tls_config(CaTlsConfig::embedded().with_extra_roots(roots));
                }
                b
            }
        })
    }

    pub(crate) fn builder(&self) -> Builder {
        match self {
            Self::Iroh => iroh::Endpoint::builder(presets::N0),
            Self::Local | Self::Relay { .. } => iroh::Endpoint::builder(presets::Minimal),
        }
    }
}

/// Where the endpoint's UDP socket listens. By default on every interface, which makes macOS ask "allow incoming
/// connections?" for each new build of the program, and exposes the port to the whole network.
///
/// `PLAYBOX_BIND=loopback`: listen on 127.0.0.1 only. No prompt, and nothing on the network can reach the host
/// directly; with `iroh` or `relay:` transports, members still connect through the relay (outbound only), and with
/// `local` only processes on this machine can. `PLAYBOX_BIND=<ip>` listens on that one address (a LAN or VPN address).
fn bind_from_env(b: Builder, value: Option<&str>) -> Result<Builder> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty() && *v != "all") else {
        return Ok(b);
    };
    let ip: std::net::IpAddr = if v == "loopback" {
        std::net::Ipv4Addr::LOCALHOST.into()
    } else {
        v.parse().with_context(|| format!("PLAYBOX_BIND must be `loopback`, `all` or an IP address, got {v:?}"))?
    };
    b.clear_ip_transports()
        .bind_addr(std::net::SocketAddr::new(ip, 0))
        .map_err(|e| anyhow!("cannot listen on {ip}: {e}"))
}

/// Which transport a joiner uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TransportChoice {
    /// Whatever the join link says.
    #[default]
    FromLink,
    Use(Transport),
}

impl From<Transport> for TransportChoice {
    fn from(t: Transport) -> Self {
        Self::Use(t)
    }
}

impl From<bool> for TransportChoice {
    /// `true` forces [`Transport::Local`]; `false` follows the link.
    fn from(local: bool) -> Self {
        if local {
            Self::Use(Transport::Local)
        } else {
            Self::FromLink
        }
    }
}

impl TransportChoice {
    #[must_use]
    pub fn resolve(self, link: Option<&Transport>) -> Transport {
        match self {
            Self::Use(t) => t,
            Self::FromLink => link.cloned().unwrap_or(Transport::Iroh),
        }
    }
}

/// Extract DER certificates from PEM text.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn pem_certs(pem: &str) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in pem.lines() {
        let line = line.trim();
        if line == "-----BEGIN CERTIFICATE-----" {
            cur = Some(String::new());
        } else if line == "-----END CERTIFICATE-----" {
            let b = cur.take().ok_or_else(|| anyhow!("END without BEGIN"))?;
            out.push(
                B64STD
                    .decode(b)
                    .context("certificate is not valid base64")?,
            );
        } else if let Some(c) = cur.as_mut() {
            c.push_str(line);
        }
    }
    if cur.is_some() {
        bail!("unterminated certificate");
    }
    if out.is_empty() {
        bail!("no certificates found");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transports() {
        assert_eq!(Transport::parse("local").unwrap(), Transport::Local);
        assert_eq!(Transport::parse("iroh").unwrap(), Transport::Iroh);
        let t = Transport::parse("relay:https://relay.example.com").unwrap();
        assert_eq!(t.label(), "relay:https://relay.example.com/");
        assert!(Transport::parse("relay:not a url").is_err());
        assert!(Transport::parse("tor").is_err());
    }

    #[test]
    fn links_are_followed_unless_overridden() {
        let r = Transport::Relay {
            url: "https://r.example/".into(),
            extra_roots: vec![],
        };
        assert_eq!(TransportChoice::FromLink.resolve(Some(&r)), r);
        assert_eq!(
            TransportChoice::from(true).resolve(Some(&r)),
            Transport::Local
        );
        assert_eq!(TransportChoice::FromLink.resolve(None), Transport::Iroh);
    }

    #[test]
    fn bind_setting_is_validated() {
        let b = || iroh::Endpoint::builder(presets::Minimal);
        assert!(bind_from_env(b(), None).is_ok());
        assert!(bind_from_env(b(), Some("all")).is_ok());
        assert!(bind_from_env(b(), Some("loopback")).is_ok());
        assert!(bind_from_env(b(), Some("192.168.1.5")).is_ok());
        assert!(bind_from_env(b(), Some("not-an-ip")).is_err());
    }

    #[test]
    fn reads_pem() {
        let pem = "junk\n-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_certs(pem).unwrap(), vec![vec![0, 1, 2]]);
        assert!(pem_certs("nothing").is_err());
    }
}
