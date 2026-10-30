//! Revocation that reaches every host of a room.
//!
//! A room's grants are signed by one root key, so any host that knows the root *public* key (a
//! replica) can check them. A revocation therefore has to be checkable by every such host too: it
//! is a [`Notice`] signed by the root key. Anyone may carry one; only the root key can make one, so
//! a replica applies a notice from any source it hears it from.
//!
//! Notices travel two ways:
//! - **gossip** (iroh-gossip) spreads a new notice quickly to every host on the room's topic;
//! - **pull** repairs what gossip missed: hosts periodically ask their configured peers for every
//!   notice they hold. A host that was offline catches up the next time it can reach a peer.
//!
//! The set of revocations only ever grows, so applying a notice twice, or in any order, is harmless.
//! There is no un-revoke. Revocation is eventually consistent: until a notice reaches a replica, that
//! replica still honours the grant. Short grant lifetimes remain the bound on that window.

use crate::{grants::Authority, State};
use anyhow::{anyhow, bail, Context, Result};
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
    Endpoint, EndpointAddr, EndpointId,
};
use iroh_gossip::{api::Event, net::Gossip, proto::TopicId};
use n0_future::StreamExt;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};

pub const SYNC_ALPN: &[u8] = b"playbox/revsync/1";
const HEADER: &str = "playbox.revoke.v1";
const MAX_REPLY: usize = 8 * 1024 * 1024;

/// A signed statement that a grant is revoked and its holder barred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The grant's authority-block revocation id (hex). Revoking it revokes every attenuation.
    pub grant_id: String,
    /// The member's endpoint id, barred from rejoining.
    pub member: String,
    pub issued_ms: u64,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex64(s: &str) -> Result<[u8; 64]> {
    if s.len() != 128 || !s.is_ascii() {
        bail!("signature must be 128 hex characters");
    }
    let mut out = [0u8; 64];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

impl Notice {
    fn body(&self, root_public: &[u8; 32]) -> String {
        format!(
            "{HEADER}\nroom {}\ngrant {}\nmember {}\nissued_ms {}\n",
            hex(root_public),
            self.grant_id,
            self.member,
            self.issued_ms
        )
    }

    /// The signed text form. Fails on a replica, which has no signing key.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn sign(&self, auth: &Authority) -> Result<String> {
        let body = self.body(&auth.public_bytes());
        let sig = auth
            .sign_notice(body.as_bytes())
            .ok_or_else(|| anyhow!("this host cannot sign revocations"))?;
        Ok(format!("{body}sig {}\n", hex(&sig)))
    }

    /// Parse a notice and check it was signed by this room's root key.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn parse(text: &str, auth: &Authority) -> Result<Notice> {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() != 6 || lines[0] != HEADER {
            bail!("not a revocation notice");
        }
        let field = |i: usize, key: &str| -> Result<&str> {
            lines[i]
                .strip_prefix(key)
                .and_then(|r| r.strip_prefix(' '))
                .ok_or_else(|| anyhow!("expected `{key}` on line {}", i + 1))
        };
        let grant_id = field(2, "grant")?;
        if grant_id.is_empty()
            || grant_id.len() > 512
            || !grant_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            bail!("bad grant id");
        }
        let member = field(3, "member")?;
        member
            .parse::<EndpointId>()
            .map_err(|e| anyhow!("bad member id: {e}"))?;
        let notice = Notice {
            grant_id: grant_id.to_string(),
            member: member.to_string(),
            issued_ms: field(4, "issued_ms")?.parse().context("bad issued_ms")?,
        };
        // Re-serialising against *our* root key means a notice for another room cannot verify here.
        let body = notice.body(&auth.public_bytes());
        if lines[..5].join("\n") + "\n" != body {
            bail!("notice is not for this room or not in canonical form");
        }
        if !auth.verify_notice(body.as_bytes(), &unhex64(field(5, "sig")?)?) {
            bail!("notice signature does not verify");
        }
        Ok(notice)
    }
}

impl State {
    /// Record a verified notice. Returns whether it was new.
    pub(crate) fn apply(&mut self, n: &Notice, text: &str) -> bool {
        if self.notices.contains_key(&n.grant_id) {
            return false;
        }
        self.notices.insert(n.grant_id.clone(), text.to_string());
        self.revoked.insert(n.grant_id.clone());
        if let Ok(id) = n.member.parse::<EndpointId>() {
            self.members.remove(&id);
            self.banned.insert(id);
        }
        true
    }
}

/// Verify `text` and apply it. `Ok(true)` if it changed anything.
pub(crate) fn apply_text(state: &Mutex<State>, auth: &Authority, text: &str) -> Result<bool> {
    let n = Notice::parse(text, auth)?;
    let mut st = state.lock().expect("state lock");
    let new = st.apply(&n, text);
    if new {
        st.save();
    }
    Ok(new)
}

fn topic(auth: &Authority) -> TopicId {
    let mut h = blake3::Hasher::new();
    h.update(b"playbox.revoke.topic.v1");
    h.update(&auth.public_bytes());
    TopicId::from_bytes(*h.finalize().as_bytes())
}

/// Serves this host's notices to the peers it was told about, and nobody else.
#[derive(Clone)]
pub(crate) struct SyncHandler {
    pub state: Arc<Mutex<State>>,
    pub allowed: Arc<Mutex<HashSet<EndpointId>>>,
}

impl std::fmt::Debug for SyncHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncHandler")
    }
}

impl ProtocolHandler for SyncHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        if !self
            .allowed
            .lock()
            .expect("allowed lock")
            .contains(&conn.remote_id())
        {
            conn.close(1u32.into(), b"not a sync peer");
            return Ok(());
        }
        let Ok((mut send, mut recv)) = conn.accept_bi().await else {
            return Ok(());
        };
        let _ = recv.read_to_end(1024).await;
        let texts: Vec<String> = self
            .state
            .lock()
            .expect("state lock")
            .notices
            .values()
            .cloned()
            .collect();
        if let Ok(body) = serde_json::to_vec(&texts) {
            let _ = send.write_all(&body).await;
            let _ = send.finish();
        }
        conn.closed().await;
        Ok(())
    }
}

/// Everything a host needs to take part: the gossip protocol, the peers it trusts to sync with, and
/// the queue of notices waiting to be broadcast.
pub(crate) struct Replication {
    pub gossip: Gossip,
    pub memory: iroh::address_lookup::memory::MemoryLookup,
    pub allowed: Arc<Mutex<HashSet<EndpointId>>>,
    pub tx: mpsc::UnboundedSender<String>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// Set once sync has started: the gossip handle and the peers the pull loop visits.
    running: tokio::sync::Mutex<Option<Running>>,
}

struct Running {
    sender: iroh_gossip::api::GossipSender,
    peers: Arc<Mutex<Vec<EndpointAddr>>>,
}

impl Replication {
    pub fn new(gossip: Gossip, memory: iroh::address_lookup::memory::MemoryLookup) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            gossip,
            memory,
            allowed: Arc::default(),
            tx,
            rx: Mutex::new(Some(rx)),
            tasks: Mutex::default(),
            running: Default::default(),
        }
    }

    pub fn stop(&self) {
        for t in self.tasks.lock().expect("tasks lock").drain(..) {
            t.abort();
        }
    }

    /// Start gossiping on the room's topic and pulling from `peers` every `interval`. Calling it again
    /// adds more peers to the running sync (the interval is fixed by the first call).
    pub async fn start(
        &self,
        endpoint: Endpoint,
        state: Arc<Mutex<State>>,
        auth: Arc<Authority>,
        peers: Vec<EndpointAddr>,
        interval: Duration,
    ) -> Result<()> {
        let ids: Vec<EndpointId> = peers.iter().map(|p| p.id).collect();
        for p in peers.iter().filter(|p| !p.addrs.is_empty()) {
            self.memory.add_endpoint_info(p.clone());
        }
        self.allowed
            .lock()
            .expect("allowed lock")
            .extend(ids.iter().copied());

        let mut running = self.running.lock().await;
        if let Some(r) = running.as_ref() {
            // Already running: remember the new peers (replacing any older address for the same host)
            // and ask gossip to dial them.
            {
                let mut known = r.peers.lock().expect("peers lock");
                for p in peers {
                    known.retain(|k| k.id != p.id);
                    known.push(p);
                }
            }
            r.sender
                .join_peers(ids)
                .await
                .context("adding gossip peers")?;
            return Ok(());
        }
        let rx = self
            .rx
            .lock()
            .expect("rx lock")
            .take()
            .ok_or_else(|| anyhow!("revocation sync is already running"))?;
        let (sender, mut receiver) = self
            .gossip
            .subscribe(topic(&auth), ids)
            .await
            .context("joining the revocation topic")?
            .split();

        // Hear notices from gossip.
        let (st, au) = (state.clone(), auth.clone());
        let hear = tokio::spawn(async move {
            while let Ok(Some(ev)) = receiver.try_next().await {
                if let Event::Received(msg) = ev {
                    if let Ok(text) = String::from_utf8(msg.content.to_vec()) {
                        let _ = apply_text(&st, &au, &text);
                    }
                }
            }
        });

        // Broadcast ours, including any we already hold.
        {
            let held: Vec<String> = state
                .lock()
                .expect("state lock")
                .notices
                .values()
                .cloned()
                .collect();
            for t in held {
                let _ = self.tx.send(t);
            }
        }
        let broadcaster = sender.clone();
        let say = tokio::spawn(async move {
            let mut rx = rx;
            while let Some(text) = rx.recv().await {
                let _ = broadcaster.broadcast(bytes::Bytes::from(text)).await;
            }
        });

        // Repair: ask peers for everything they have. Peers without a known address (only an id) are
        // skipped; they will pull from us.
        let shared = Arc::new(Mutex::new(peers));
        let visit = shared.clone();
        let pull = tokio::spawn(async move {
            loop {
                let current: Vec<EndpointAddr> = visit
                    .lock()
                    .expect("peers lock")
                    .iter()
                    .filter(|p| !p.addrs.is_empty())
                    .cloned()
                    .collect();
                for peer in &current {
                    let _ = pull_from(&endpoint, &state, &auth, peer).await;
                }
                tokio::time::sleep(interval).await;
            }
        });
        self.tasks
            .lock()
            .expect("tasks lock")
            .extend([hear, say, pull]);
        *running = Some(Running {
            sender,
            peers: shared,
        });
        Ok(())
    }
}

/// Ask one peer for its notices and apply them. Returns how many were new.
pub(crate) async fn pull_from(
    endpoint: &Endpoint,
    state: &Mutex<State>,
    auth: &Authority,
    peer: &EndpointAddr,
) -> Result<usize> {
    let conn = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(peer.clone(), SYNC_ALPN),
    )
    .await
    .map_err(|_| anyhow!("timed out connecting to {}", peer.id.fmt_short()))??;
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(b"?").await?;
    send.finish()?;
    let reply = recv.read_to_end(MAX_REPLY).await?;
    conn.close(0u32.into(), b"done");
    let texts: Vec<String> = serde_json::from_slice(&reply).context("bad sync reply")?;
    let mut new = 0;
    for t in texts {
        // A bad notice from a peer is skipped, never trusted and never fatal.
        if apply_text(state, auth, &t).unwrap_or(false) {
            new += 1;
        }
    }
    Ok(new)
}
