//! Join links (`playbox://...#invite`) and copy-pasteable peer addresses.

use crate::keys::{identity_key, new_secret_key};
use crate::{Transport, LINK_SCHEME};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};

/// Most other hosts a link carries.
pub const MAX_HINTS: usize = 5;
/// Maximum encoded byte length of a `playbox://...#invite` link accepted by [`Link::parse`].
pub const MAX_LINK_BYTES: usize = 32 * 1024;
/// Maximum encoded byte length of a copy-pasteable peer address accepted by [`decode_peer`].
pub const MAX_PEER_BYTES: usize = 8 * 1024;

/// `playbox://<base64url(json({addr, t}))>#<invite>`, where `t` names the host's transport.
/// Older links held just the address; those parse with no transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub addr: EndpointAddr,
    pub invite: String,
    pub transport: Option<Transport>,
    /// The invite works only for the key the host was told about (see [`LinkOpts::bind_to`]). The guest
    /// then joins with its stable identity key instead of a fresh one.
    pub bound: bool,
    /// Other hosts of the room, so a guest has somewhere to go if the first is down. Older links have none.
    pub hosts: Vec<EndpointAddr>,
}

/// How a join link is made.
#[derive(Debug, Clone, Default)]
pub struct LinkOpts {
    /// Redeemable only by this key. A link that leaks is then useless to anyone else.
    pub bind_to: Option<EndpointId>,
    /// Put the host's IP addresses in the link. By default they are left out on `iroh` and `relay:`
    /// transports (the guest finds the host through its relay and discovery) so a shared link does not
    /// reveal where the host is. `local` links always carry them, since there is no relay to go through.
    pub show_ips: bool,
}

/// The key to join `link` with: a fresh one, or `identity` when the link is bound to a key.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn key_for_link(link: &str, identity: &std::path::Path) -> Result<SecretKey> {
    if Link::parse(link)?.bound {
        identity_key(identity)
    } else {
        new_secret_key()
    }
}

/// A host's address as one copy-pasteable string, for `--sync-peer`.
///
/// # Panics
///
/// Panics only if JSON serialization fails, which cannot happen for this plain data type.
#[must_use]
pub fn encode_peer(addr: &EndpointAddr) -> String {
    B64.encode(serde_json::to_vec(addr).expect("EndpointAddr serializes"))
}

/// Either a full address from [`encode_peer`], or just an endpoint id (64 hex characters). An id alone
/// lets that host pull notices from us and join the gossip, but we cannot dial it until it contacts us.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn decode_peer(s: &str) -> Result<EndpointAddr> {
    let s = s.trim();
    if s.len() > MAX_PEER_BYTES {
        bail!("encoded peer address exceeds {MAX_PEER_BYTES} bytes");
    }
    if let Ok(id) = s.parse::<EndpointId>() {
        return Ok(EndpointAddr::new(id));
    }
    let json = B64
        .decode(s)
        .context("peer is neither an endpoint id nor an encoded address")?;
    serde_json::from_slice(&json).context("peer address is not valid")
}

#[derive(Serialize, Deserialize)]
struct LinkBody {
    addr: EndpointAddr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    t: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    b: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    h: Vec<EndpointAddr>,
}

impl Link {
    /// # Panics
    ///
    /// Panics only if JSON serialization fails, which cannot happen for this plain data type.
    pub fn encode(&self) -> String {
        let body = LinkBody {
            addr: self.addr.clone(),
            t: self.transport.as_ref().map(Transport::label),
            b: self.bound,
            h: self.hosts.iter().take(MAX_HINTS).cloned().collect(),
        };
        let json = serde_json::to_vec(&body).expect("link body serializes");
        format!("{LINK_SCHEME}{}#{}", B64.encode(json), self.invite)
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() > MAX_LINK_BYTES {
            bail!("link exceeds {MAX_LINK_BYTES} bytes");
        }
        let rest = s
            .strip_prefix(LINK_SCHEME)
            .ok_or_else(|| anyhow!("link must start with {LINK_SCHEME}"))?;
        let (body, invite) = rest
            .split_once('#')
            .ok_or_else(|| anyhow!("link has no invite fragment"))?;
        if invite.is_empty()
            || invite
                .bytes()
                .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        {
            bail!("empty or invalid invite");
        }
        let json = B64.decode(body).context("link is not valid base64")?;
        let (addr, t, bound, hosts) = match serde_json::from_slice::<LinkBody>(&json) {
            Ok(b) => (b.addr, b.t, b.b, b.h),
            Err(_) => (
                serde_json::from_slice(&json).context("link address is not valid")?,
                None,
                false,
                Vec::new(),
            ),
        };
        let transport = t.map(|t| Transport::parse(&t)).transpose()?;
        Ok(Self {
            addr,
            invite: invite.to_string(),
            transport,
            bound,
            hosts: hosts.into_iter().take(MAX_HINTS).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_and_peer_parsers_enforce_size_and_invite_character_bounds() {
        let huge_link = format!("playbox://{}#inv", "A".repeat(MAX_LINK_BYTES + 16));
        assert!(Link::parse(&huge_link).is_err());

        let huge_peer = "A".repeat(MAX_PEER_BYTES + 16);
        assert!(decode_peer(&huge_peer).is_err());

        let bad_invite = "playbox://e30#inv\nwith-newline";
        assert!(Link::parse(bad_invite).is_err());
        let space_invite = "playbox://e30#inv with space";
        assert!(Link::parse(space_invite).is_err());
    }
}
