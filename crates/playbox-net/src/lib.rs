//! Playbox networking (M1): a host that admits members by invite and a client that joins by link.
//!
//! The wire format here is a placeholder JSON request/response over one iroh stream per request.
//! It is not yet the A2A binding; see PRD section 6 and risk 1.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use iroh::{
    endpoint::{presets, Connection},
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointAddr, EndpointId, SecretKey,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const ALPN: &[u8] = b"playbox/room/0";
const MAX_MSG: usize = 64 * 1024;
const LINK_SCHEME: &str = "playbox://";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 32 random bytes from the OS.
pub fn random_bytes() -> Result<[u8; 32]> {
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

pub fn new_secret_key() -> Result<SecretKey> {
    Ok(SecretKey::from_bytes(&random_bytes()?))
}

/// `playbox://<base64url(json(EndpointAddr))>#<invite>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub addr: EndpointAddr,
    pub invite: String,
}

impl Link {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(&self.addr).expect("EndpointAddr serializes");
        format!("{LINK_SCHEME}{}#{}", B64.encode(json), self.invite)
    }

    pub fn parse(s: &str) -> Result<Self> {
        let rest = s.strip_prefix(LINK_SCHEME).ok_or_else(|| anyhow!("link must start with {LINK_SCHEME}"))?;
        let (addr, invite) = rest.split_once('#').ok_or_else(|| anyhow!("link has no invite fragment"))?;
        let json = B64.decode(addr).context("link address is not valid base64")?;
        let addr = serde_json::from_slice(&json).context("link address is not valid")?;
        if invite.is_empty() {
            bail!("empty invite");
        }
        Ok(Self { addr, invite: invite.to_string() })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum Request {
    Join { invite: String },
    Echo { text: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum Response {
    Joined { member: String },
    Echo { text: String, from: String },
    Error { message: String },
}

#[derive(Debug)]
struct Invite {
    expires_ms: u64,
    uses_left: u32,
}

#[derive(Debug, Default)]
struct State {
    invites: HashMap<String, Invite>,
    members: HashSet<EndpointId>,
}

#[derive(Debug, Clone)]
struct RoomHandler {
    state: Arc<Mutex<State>>,
}

impl RoomHandler {
    fn handle(&self, from: EndpointId, req: Request) -> Response {
        let mut st = self.state.lock().expect("state lock");
        match req {
            Request::Join { invite } => {
                let Some(inv) = st.invites.get_mut(&invite) else {
                    return Response::Error { message: "unknown invite".into() };
                };
                if now_ms() >= inv.expires_ms {
                    st.invites.remove(&invite);
                    return Response::Error { message: "invite expired".into() };
                }
                inv.uses_left -= 1;
                if inv.uses_left == 0 {
                    st.invites.remove(&invite);
                }
                st.members.insert(from);
                Response::Joined { member: from.to_string() }
            }
            Request::Echo { text } => {
                if !st.members.contains(&from) {
                    return Response::Error { message: "not a member".into() };
                }
                Response::Echo { text, from: from.to_string() }
            }
        }
    }
}

impl ProtocolHandler for RoomHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let from = conn.remote_id();
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let Ok(bytes) = recv.read_to_end(MAX_MSG).await else { break };
            let resp = match serde_json::from_slice::<Request>(&bytes) {
                Ok(req) => self.handle(from, req),
                Err(e) => Response::Error { message: format!("bad request: {e}") },
            };
            let out = serde_json::to_vec(&resp).expect("response serializes");
            if send.write_all(&out).await.is_err() || send.finish().is_err() {
                break;
            }
        }
        conn.closed().await;
        Ok(())
    }
}

async fn bind(secret: SecretKey, local: bool) -> Result<Endpoint> {
    let alpns = vec![ALPN.to_vec()];
    let ep = if local {
        Endpoint::builder(presets::Minimal).secret_key(secret).alpns(alpns).bind().await?
    } else {
        Endpoint::builder(presets::N0).secret_key(secret).alpns(alpns).bind().await?
    };
    Ok(ep)
}

pub struct Host {
    router: Router,
    state: Arc<Mutex<State>>,
}

impl Host {
    /// `local` skips relays and discovery: direct addresses only (tests, LAN).
    pub async fn start(secret: SecretKey, local: bool) -> Result<Self> {
        let endpoint = bind(secret, local).await?;
        if !local {
            endpoint.online().await;
        }
        let state = Arc::new(Mutex::new(State::default()));
        let router = Router::builder(endpoint).accept(ALPN, RoomHandler { state: state.clone() }).spawn();
        Ok(Self { router, state })
    }

    pub fn addr(&self) -> EndpointAddr {
        self.router.endpoint().addr()
    }

    /// Create a join link valid for `ttl` and `uses` joins.
    pub fn create_link(&self, ttl: Duration, uses: u32) -> Result<Link> {
        let invite: String = random_bytes()?[..16].iter().map(|b| format!("{b:02x}")).collect();
        self.state.lock().expect("state lock").invites.insert(
            invite.clone(),
            Invite { expires_ms: now_ms() + ttl.as_millis() as u64, uses_left: uses.max(1) },
        );
        Ok(Link { addr: self.addr(), invite })
    }

    pub fn members(&self) -> Vec<EndpointId> {
        self.state.lock().expect("state lock").members.iter().copied().collect()
    }

    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await.map_err(|e| anyhow!("{e}"))
    }
}

pub struct Client {
    endpoint: Endpoint,
    conn: Connection,
}

impl Client {
    pub async fn connect(addr: EndpointAddr, secret: SecretKey, local: bool) -> Result<Self> {
        let endpoint = bind(secret, local).await?;
        let conn = endpoint.connect(addr, ALPN).await?;
        Ok(Self { endpoint, conn })
    }

    async fn call(&self, req: &Request) -> Result<Response> {
        let (mut send, mut recv) = self.conn.open_bi().await?;
        send.write_all(&serde_json::to_vec(req)?).await?;
        send.finish()?;
        let bytes = recv.read_to_end(MAX_MSG).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Present the invite; returns this client's member id.
    pub async fn join(&self, invite: &str) -> Result<String> {
        match self.call(&Request::Join { invite: invite.to_string() }).await? {
            Response::Joined { member } => Ok(member),
            Response::Error { message } => bail!("join refused: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub async fn echo(&self, text: &str) -> Result<String> {
        match self.call(&Request::Echo { text: text.to_string() }).await? {
            Response::Echo { text, .. } => Ok(text),
            Response::Error { message } => bail!("echo refused: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub async fn close(self) {
        self.conn.close(0u32.into(), b"bye");
        self.endpoint.close().await;
    }
}

/// Parse a link, connect, and join in one step.
pub async fn join_link(link: &str, secret: SecretKey, local: bool) -> Result<Client> {
    let link = Link::parse(link)?;
    let client = Client::connect(link.addr, secret, local).await?;
    client.join(&link.invite).await?;
    Ok(client)
}
