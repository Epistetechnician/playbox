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
use playbox_runner::{Limits, Runner, TaskOutput, TaskSpec};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

pub const ALPN: &[u8] = b"playbox/room/0";
/// Header + body cap for one request or response (bundles and patches travel in the body).
const MAX_FRAME: usize = 256 * 1024 * 1024;
const MAX_HEADER: usize = 64 * 1024;
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
    /// Fetch `refname` and its history as a git bundle (response body).
    Pull { refname: String },
    /// Send a git bundle (request body) holding `branch`; stored under the member's own namespace.
    Push { branch: String },
    /// Run `spec` in a sandbox on a checkout of `base`; the response body is the resulting patch.
    RunTask { base: String, spec: TaskSpec },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum Response {
    Joined { member: String },
    Echo { text: String, from: String },
    Pulled { head: String },
    Pushed { reference: String, head: String },
    TaskDone { runner: String, output: TaskOutput },
    Error { message: String },
}

/// Frame: `u32 big-endian header length | header json | body`.
fn encode_frame(header: &impl Serialize, body: &[u8]) -> Result<Vec<u8>> {
    let h = serde_json::to_vec(header)?;
    let mut out = Vec::with_capacity(4 + h.len() + body.len());
    out.extend_from_slice(&(h.len() as u32).to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(body);
    Ok(out)
}

fn decode_frame<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<(T, Vec<u8>)> {
    let len = u32::from_be_bytes(bytes.get(..4).ok_or_else(|| anyhow!("short frame"))?.try_into()?) as usize;
    if len > MAX_HEADER {
        bail!("header too large");
    }
    let h = bytes.get(4..4 + len).ok_or_else(|| anyhow!("truncated header"))?;
    Ok((serde_json::from_slice(h)?, bytes[4 + len..].to_vec()))
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

/// What a host offers beyond membership. Anything left `None` is refused.
#[derive(Clone, Default)]
pub struct HostConfig {
    /// The room's git repository (a normal or bare repo).
    pub repo: Option<PathBuf>,
    /// Where member tasks run. Without one, `RunTask` is refused.
    pub runner: Option<Arc<dyn Runner>>,
    /// Host-side caps; a member's requested limits are clamped to these.
    pub limits: Limits,
}

#[derive(Clone)]
struct RoomHandler {
    state: Arc<Mutex<State>>,
    cfg: HostConfig,
    tasks: Arc<Semaphore>,
}

impl std::fmt::Debug for RoomHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomHandler")
    }
}

fn err(message: impl Into<String>) -> (Response, Vec<u8>) {
    (Response::Error { message: message.into() }, Vec::new())
}

impl RoomHandler {
    fn is_member(&self, id: &EndpointId) -> bool {
        self.state.lock().expect("state lock").members.contains(id)
    }

    fn join(&self, from: EndpointId, invite: String) -> Response {
        let mut st = self.state.lock().expect("state lock");
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

    async fn handle(&self, from: EndpointId, req: Request, body: Vec<u8>) -> (Response, Vec<u8>) {
        if let Request::Join { invite } = req {
            return (self.join(from, invite), Vec::new());
        }
        if !self.is_member(&from) {
            return err("not a member");
        }
        match self.handle_member(from, req, body).await {
            Ok(r) => r,
            Err(e) => err(format!("{e:#}")),
        }
    }

    async fn handle_member(&self, from: EndpointId, req: Request, body: Vec<u8>) -> Result<(Response, Vec<u8>)> {
        let repo = || self.cfg.repo.clone().ok_or_else(|| anyhow!("this room has no git repository"));
        Ok(match req {
            Request::Join { .. } => unreachable!("handled above"),
            Request::Echo { text } => (Response::Echo { text, from: from.to_string() }, Vec::new()),
            Request::Pull { refname } => {
                let repo = repo()?;
                let head = playbox_git::rev_parse(&repo, &refname).await?;
                let bundle = playbox_git::bundle_create(&repo, &refname).await?;
                (Response::Pulled { head }, bundle)
            }
            Request::Push { branch } => {
                let repo = repo()?;
                let short: String = from.to_string().chars().take(16).collect();
                let reference = format!("refs/playbox/members/{short}/{}", playbox_git::check_name(&branch)?);
                let src = format!("refs/heads/{branch}");
                let head = playbox_git::bundle_import(&repo, &body, &src, &reference).await?;
                (Response::Pushed { reference, head }, Vec::new())
            }
            Request::RunTask { base, spec } => {
                let repo = repo()?;
                let runner = self.cfg.runner.clone().ok_or_else(|| anyhow!("this room runs no tasks"))?;
                let _permit = self.tasks.acquire().await?;
                let spec = spec.clamp(&self.cfg.limits);
                let commit = playbox_git::rev_parse(&repo, &base).await?;
                let dir = playbox_git::worktree_add(&repo, &commit).await?;
                let result = async {
                    let output = runner.run(&spec, &dir).await?;
                    let patch = playbox_git::diff_patch(&dir, &commit).await?;
                    anyhow::Ok((output, patch))
                }
                .await;
                let _ = playbox_git::worktree_remove(&repo, &dir).await;
                let (output, patch) = result?;
                (Response::TaskDone { runner: runner.name().into(), output }, patch)
            }
        })
    }
}

impl ProtocolHandler for RoomHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let from = conn.remote_id();
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let Ok(bytes) = recv.read_to_end(MAX_FRAME).await else { break };
            let (resp, body) = match decode_frame::<Request>(&bytes) {
                Ok((req, body)) => self.handle(from, req, body).await,
                Err(e) => err(format!("bad request: {e}")),
            };
            let Ok(out) = encode_frame(&resp, &body) else { break };
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
        Self::start_with(secret, local, HostConfig::default()).await
    }

    pub async fn start_with(secret: SecretKey, local: bool, cfg: HostConfig) -> Result<Self> {
        let endpoint = bind(secret, local).await?;
        if !local {
            endpoint.online().await;
        }
        let state = Arc::new(Mutex::new(State::default()));
        let handler = RoomHandler { state: state.clone(), cfg, tasks: Arc::new(Semaphore::new(2)) };
        let router = Router::builder(endpoint).accept(ALPN, handler).spawn();
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

    async fn call(&self, req: &Request, body: &[u8]) -> Result<(Response, Vec<u8>)> {
        let (mut send, mut recv) = self.conn.open_bi().await?;
        send.write_all(&encode_frame(req, body)?).await?;
        send.finish()?;
        decode_frame(&recv.read_to_end(MAX_FRAME).await?)
    }

    /// Present the invite; returns this client's member id.
    pub async fn join(&self, invite: &str) -> Result<String> {
        match self.call(&Request::Join { invite: invite.to_string() }, &[]).await?.0 {
            Response::Joined { member } => Ok(member),
            Response::Error { message } => bail!("join refused: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub async fn echo(&self, text: &str) -> Result<String> {
        match self.call(&Request::Echo { text: text.to_string() }, &[]).await?.0 {
            Response::Echo { text, .. } => Ok(text),
            Response::Error { message } => bail!("echo refused: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Fetch `refname` from the room; returns its commit id and a git bundle.
    pub async fn pull(&self, refname: &str) -> Result<(String, Vec<u8>)> {
        match self.call(&Request::Pull { refname: refname.to_string() }, &[]).await? {
            (Response::Pulled { head }, bundle) => Ok((head, bundle)),
            (Response::Error { message }, _) => bail!("pull refused: {message}"),
            (other, _) => bail!("unexpected response: {other:?}"),
        }
    }

    /// Send a bundle holding `branch`. Returns the ref it was stored under on the host.
    pub async fn push(&self, branch: &str, bundle: &[u8]) -> Result<String> {
        match self.call(&Request::Push { branch: branch.to_string() }, bundle).await?.0 {
            Response::Pushed { reference, .. } => Ok(reference),
            Response::Error { message } => bail!("push refused: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Run a task on a checkout of `base`; returns the runner name, its output and the patch.
    pub async fn run_task(&self, base: &str, spec: TaskSpec) -> Result<(String, TaskOutput, Vec<u8>)> {
        match self.call(&Request::RunTask { base: base.to_string(), spec }, &[]).await? {
            (Response::TaskDone { runner, output }, patch) => Ok((runner, output, patch)),
            (Response::Error { message }, _) => bail!("task refused: {message}"),
            (other, _) => bail!("unexpected response: {other:?}"),
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
