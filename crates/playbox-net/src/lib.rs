//! Playbox networking (M1-M3): a host that admits members by invite and a client that joins by link.
//!
//! The wire format here is a placeholder JSON request/response over one iroh stream per request.
//! It is not yet the A2A binding; see PRD section 6 and risk 1.

pub mod grants;
pub mod transport;
pub use transport::{Transport, TransportChoice};

use anyhow::{anyhow, bail, Context, Result};
use grants::{Authority, Caps, Needs};
pub use iroh::EndpointId;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointAddr, SecretKey,
};
use playbox_runner::{Limits, Runner, TaskOutput, TaskSpec};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

pub const ALPN: &[u8] = b"playbox/room/0";
/// Header + body cap for one request or response (bundles and patches travel in the body).
const MAX_FRAME: usize = 256 * 1024 * 1024;
const MAX_HEADER: usize = 64 * 1024;
const CONNECT_ATTEMPTS: u32 = 4;
const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(6);
const LINK_SCHEME: &str = "playbox://";

/// 32 random bytes from the OS.
pub fn random_bytes() -> Result<[u8; 32]> {
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

pub fn new_secret_key() -> Result<SecretKey> {
    Ok(SecretKey::from_bytes(&random_bytes()?))
}

/// `playbox://<base64url(json({addr, t}))>#<invite>`, where `t` names the host's transport.
/// Older links held just the address; those parse with no transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub addr: EndpointAddr,
    pub invite: String,
    pub transport: Option<Transport>,
}

#[derive(Serialize, Deserialize)]
struct LinkBody {
    addr: EndpointAddr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    t: Option<String>,
}

impl Link {
    pub fn encode(&self) -> String {
        let body = LinkBody { addr: self.addr.clone(), t: self.transport.as_ref().map(Transport::label) };
        let json = serde_json::to_vec(&body).expect("link body serializes");
        format!("{LINK_SCHEME}{}#{}", B64.encode(json), self.invite)
    }

    pub fn parse(s: &str) -> Result<Self> {
        let rest = s.strip_prefix(LINK_SCHEME).ok_or_else(|| anyhow!("link must start with {LINK_SCHEME}"))?;
        let (body, invite) = rest.split_once('#').ok_or_else(|| anyhow!("link has no invite fragment"))?;
        let json = B64.decode(body).context("link is not valid base64")?;
        let (addr, t) = match serde_json::from_slice::<LinkBody>(&json) {
            Ok(b) => (b.addr, b.t),
            Err(_) => (serde_json::from_slice(&json).context("link address is not valid")?, None),
        };
        if invite.is_empty() {
            bail!("empty invite");
        }
        let transport = t.map(|t| Transport::parse(&t)).transpose()?;
        Ok(Self { addr, invite: invite.to_string(), transport })
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
    Joined { member: String, grant: String },
    Echo { text: String, from: String },
    Pulled { head: String },
    Pushed { reference: String, head: String },
    TaskDone { runner: String, output: TaskOutput },
    Error { message: String },
}

/// Every request carries the sender's grant (except `join`, which carries an invite).
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    grant: Option<String>,
    #[serde(flatten)]
    req: Request,
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
    uses_left: u32,
}

#[derive(Debug, Default)]
struct State {
    /// Unspent invites by id.
    invites: HashMap<String, Invite>,
    /// Members and the id of the grant issued to each.
    members: HashMap<EndpointId, String>,
    /// Revoked grant ids. Revoking an authority block revokes every attenuation of it.
    revoked: HashSet<String>,
    /// Members who may not rejoin.
    banned: HashSet<EndpointId>,
    /// Where this state is saved after every change, if the host keeps state on disk.
    path: Option<PathBuf>,
}

/// On-disk form of [`State`].
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    invites: HashMap<String, u32>,
    members: HashMap<String, String>,
    revoked: Vec<String>,
    banned: Vec<String>,
}

impl State {
    fn load(path: &std::path::Path) -> Result<Self> {
        let mut st = State { path: Some(path.to_path_buf()), ..Default::default() };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(st),
            Err(e) => return Err(e).context("reading host state"),
        };
        let saved: Saved = serde_json::from_str(&text).context("host state file is corrupt")?;
        let id = |s: &str| s.parse::<EndpointId>().map_err(|e| anyhow!("bad endpoint id in state: {e}"));
        st.invites = saved.invites.into_iter().map(|(k, v)| (k, Invite { uses_left: v })).collect();
        for (m, g) in saved.members {
            st.members.insert(id(&m)?, g);
        }
        st.revoked = saved.revoked.into_iter().collect();
        for b in saved.banned {
            st.banned.insert(id(&b)?);
        }
        Ok(st)
    }

    /// Write the state next to its final path, then rename, so a crash never leaves half a file.
    /// A failure is logged, not fatal: the room keeps working but the change may not survive a restart.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let saved = Saved {
            invites: self.invites.iter().map(|(k, v)| (k.clone(), v.uses_left)).collect(),
            members: self.members.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            revoked: self.revoked.iter().cloned().collect(),
            banned: self.banned.iter().map(ToString::to_string).collect(),
        };
        let tmp = path.with_extension("tmp");
        let result = serde_json::to_vec_pretty(&saved)
            .map_err(anyhow::Error::from)
            .and_then(|b| std::fs::write(&tmp, b).map_err(Into::into))
            .and_then(|_| std::fs::rename(&tmp, path).map_err(Into::into));
        if let Err(e) = result {
            eprintln!("playbox: could not save host state to {}: {e:#}", path.display());
        }
    }
}

/// A 32-byte secret stored as hex in a file readable only by the owner; created if missing.
fn load_or_create_seed(path: &std::path::Path) -> Result<[u8; 32]> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    if let Ok(t) = std::fs::read_to_string(path) {
        let t = t.trim();
        if t.len() != 64 || !t.is_ascii() {
            bail!("{} must hold 64 hex characters", path.display());
        }
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).context("key file is not hex")?;
        }
        return Ok(out);
    }
    let seed = random_bytes()?;
    let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?.write_all(hex.as_bytes())?;
    Ok(seed)
}

/// What a host offers beyond membership. Anything left `None` is refused.
#[derive(Clone, Default)]
pub struct HostConfig {
    /// The room's git repository (a normal or bare repo).
    pub repo: Option<PathBuf>,
    /// Where member tasks run. Without one, `RunTask` is refused.
    pub runner: Option<Arc<dyn Runner>>,
    /// Limits written into every member grant. A task asking for more is refused.
    pub limits: Limits,
    /// How long member grants last. Defaults to one hour.
    pub grant_ttl: Option<Duration>,
}

#[derive(Clone)]
struct RoomHandler {
    state: Arc<Mutex<State>>,
    cfg: HostConfig,
    tasks: Arc<Semaphore>,
    auth: Arc<Authority>,
}

impl std::fmt::Debug for RoomHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomHandler")
    }
}

fn err(message: impl Into<String>) -> (Response, Vec<u8>) {
    (Response::Error { message: message.into() }, Vec::new())
}

fn needs_of(req: &Request) -> Needs {
    match req {
        Request::RunTask { spec, .. } => Needs {
            cpu_milli: (spec.cpus * 1000.0).ceil() as i64,
            mem_mib: spec.mem_mib as i64,
            timeout_secs: spec.timeout_secs as i64,
            net: spec.network,
            cmd: spec.cmd.first().cloned().unwrap_or_default(),
        },
        _ => Needs::default(),
    }
}

fn op_of(req: &Request) -> &'static str {
    match req {
        Request::Join { .. } => "join",
        Request::Echo { .. } => "echo",
        Request::Pull { .. } => "pull",
        Request::Push { .. } => "push",
        Request::RunTask { .. } => "run",
    }
}

impl RoomHandler {
    fn join(&self, from: EndpointId, invite: String) -> Result<Response> {
        if self.state.lock().expect("state lock").banned.contains(&from) {
            bail!("this key is banned from the room");
        }
        let id = self.auth.verify_invite(&invite)?;
        let mut st = self.state.lock().expect("state lock");
        let inv = st.invites.get_mut(&id).ok_or_else(|| anyhow!("invite already used or unknown"))?;
        inv.uses_left -= 1;
        if inv.uses_left == 0 {
            st.invites.remove(&id);
        }
        let l = &self.cfg.limits;
        let caps = Caps {
            cpu_milli: (l.cpus * 1000.0).ceil() as i64,
            mem_mib: l.mem_mib as i64,
            timeout_secs: l.timeout_secs as i64,
            net: l.network,
        };
        let ttl = self.cfg.grant_ttl.unwrap_or(Duration::from_secs(3600)).as_secs() as i64;
        let (grant, grant_id) = self.auth.mint_grant(&from.to_string(), ttl, &caps)?;
        st.members.insert(from, grant_id);
        st.save();
        Ok(Response::Joined { member: from.to_string(), grant })
    }

    async fn handle(&self, from: EndpointId, env: Envelope, body: Vec<u8>) -> (Response, Vec<u8>) {
        let result = match env.req {
            Request::Join { invite } => self.join(from, invite).map(|r| (r, Vec::new())),
            req => {
                let authorized = match &env.grant {
                    None => Err(anyhow!("no grant presented")),
                    Some(g) => {
                        let revoked = self.state.lock().expect("state lock").revoked.clone();
                        self.auth.authorize(g, &from.to_string(), op_of(&req), &needs_of(&req), &revoked)
                    }
                };
                match authorized {
                    Ok(()) => self.handle_member(from, req, body).await,
                    Err(e) => Err(e),
                }
            }
        };
        result.unwrap_or_else(|e| err(format!("{e:#}")))
    }

    async fn handle_member(&self, from: EndpointId, req: Request, body: Vec<u8>) -> Result<(Response, Vec<u8>)> {
        let repo = || self.cfg.repo.clone().ok_or_else(|| anyhow!("this room has no git repository"));
        Ok(match req {
            Request::Join { .. } => unreachable!("handled in handle"),
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
            let (resp, body) = match decode_frame::<Envelope>(&bytes) {
                Ok((env, body)) => self.handle(from, env, body).await,
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

async fn bind(secret: SecretKey, transport: &Transport) -> Result<Endpoint> {
    let b = transport.configure(transport.builder())?;
    Ok(b.secret_key(secret).alpns(vec![ALPN.to_vec()]).bind().await?)
}

pub struct Host {
    router: Router,
    state: Arc<Mutex<State>>,
    auth: Arc<Authority>,
    transport: Transport,
}

impl Host {
    /// `transport` may be a [`Transport`] or a bool (`true` = local, `false` = public iroh).
    pub async fn start(secret: SecretKey, transport: impl Into<Transport>) -> Result<Self> {
        Self::start_with(secret, transport, HostConfig::default()).await
    }

    pub async fn start_with(secret: SecretKey, transport: impl Into<Transport>, cfg: HostConfig) -> Result<Self> {
        Self::build(secret, transport.into(), cfg, None, State::default()).await
    }

    /// Start a host that keeps its identity, signing key, invites, members and bans in `dir`.
    /// Restarting with the same `dir` gives the same endpoint id, keeps issued grants valid, and keeps
    /// revoked members out. Links created before the restart still work if their address does.
    pub async fn open(dir: &std::path::Path, transport: impl Into<Transport>, cfg: HostConfig) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let secret = SecretKey::from_bytes(&load_or_create_seed(&dir.join("endpoint.key"))?);
        let root_seed = load_or_create_seed(&dir.join("root.key"))?;
        let state = State::load(&dir.join("state.json"))?;
        Self::build(secret, transport.into(), cfg, Some(root_seed), state).await
    }

    async fn build(
        secret: SecretKey,
        transport: Transport,
        cfg: HostConfig,
        root_seed: Option<[u8; 32]>,
        state: State,
    ) -> Result<Self> {
        let endpoint = bind(secret, &transport).await?;
        if !transport.needs_online_wait() {
            // No relay to wait for: wait until the endpoint knows at least one direct address,
            // or links created right after start could carry none.
            for _ in 0..100 {
                if !endpoint.addr().addrs.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } else if tokio::time::timeout(Duration::from_secs(20), endpoint.online()).await.is_err() {
            bail!(
                "could not reach the relay for transport `{}` within 20s; check your network, or use `local`",
                transport.label()
            );
        }
        let state = Arc::new(Mutex::new(state));
        let room = endpoint.id().to_string();
        let auth = Arc::new(match root_seed {
            Some(seed) => Authority::from_seed(&room, &seed)?,
            None => Authority::new(&room),
        });
        let handler = RoomHandler { state: state.clone(), cfg, tasks: Arc::new(Semaphore::new(2)), auth: auth.clone() };
        let router = Router::builder(endpoint).accept(ALPN, handler).spawn();
        Ok(Self { router, state, auth, transport })
    }

    pub fn addr(&self) -> EndpointAddr {
        let mut addr = self.router.endpoint().addr();
        if self.transport == Transport::Local {
            // iroh lists only LAN interface addresses. A peer on this same machine would then reach
            // us by looping back through the Wi-Fi/Ethernet interface, which a host firewall can
            // intermittently drop. Offer plain loopback as well.
            let loopback: Vec<_> = addr
                .ip_addrs()
                .filter(|a| a.is_ipv4())
                .map(|a| std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, a.port())))
                .collect();
            for a in loopback {
                addr.addrs.insert(iroh::TransportAddr::Ip(a));
            }
        }
        addr
    }

    /// Create a join link whose invite lasts `ttl` and admits `uses` joins.
    pub fn create_link(&self, ttl: Duration, uses: u32) -> Result<Link> {
        let (invite, id) = self.auth.mint_invite(ttl.as_secs() as i64)?;
        {
            let mut st = self.state.lock().expect("state lock");
            st.invites.insert(id, Invite { uses_left: uses.max(1) });
            st.save();
        }
        Ok(Link { addr: self.addr(), invite, transport: Some(self.transport.clone()) })
    }

    pub fn members(&self) -> Vec<EndpointId> {
        self.state.lock().expect("state lock").members.keys().copied().collect()
    }

    /// Revoke a member's grant (and every attenuation of it) and bar the key from rejoining.
    /// Takes effect on the member's next request. Returns false for an unknown member.
    pub fn revoke_member(&self, id: &EndpointId) -> bool {
        let mut st = self.state.lock().expect("state lock");
        let Some(grant_id) = st.members.remove(id) else { return false };
        st.revoked.insert(grant_id);
        st.banned.insert(*id);
        st.save();
        true
    }

    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await.map_err(|e| anyhow!("{e}"))
    }
}

pub struct Client {
    endpoint: Endpoint,
    conn: Connection,
    grant: Mutex<Option<String>>,
}

impl Client {
    pub async fn connect(addr: EndpointAddr, secret: SecretKey, transport: impl Into<Transport>) -> Result<Self> {
        let endpoint = bind(secret, &transport.into()).await?;
        // A handshake that never completes is retried on the same endpoint: in practice a stuck
        // first attempt is far more common than a host that is really unreachable.
        let mut last = anyhow!("no connection attempt was made");
        let mut conn = None;
        for _ in 0..CONNECT_ATTEMPTS {
            match tokio::time::timeout(CONNECT_ATTEMPT_TIMEOUT, endpoint.connect(addr.clone(), ALPN)).await {
                Ok(Ok(c)) => {
                    conn = Some(c);
                    break;
                }
                Ok(Err(e)) => last = e.into(),
                Err(_) => last = anyhow!("timed out after {}s", CONNECT_ATTEMPT_TIMEOUT.as_secs()),
            }
        }
        let conn = conn.ok_or_else(|| last.context(format!("could not connect after {CONNECT_ATTEMPTS} attempts")))?;
        Ok(Self { endpoint, conn, grant: Mutex::new(None) })
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// The grant this client presents, if it has joined.
    pub fn grant(&self) -> Option<String> {
        self.grant.lock().expect("grant lock").clone()
    }

    pub fn set_grant(&self, grant: String) {
        *self.grant.lock().expect("grant lock") = Some(grant);
    }

    /// Narrow this client's grant with extra Biscuit checks. Works offline and cannot be undone.
    /// Example: `check if operation("pull");`
    pub fn attenuate(&self, checks: &str) -> Result<()> {
        let g = self.grant().ok_or_else(|| anyhow!("not joined"))?;
        self.set_grant(grants::attenuate(&g, checks)?);
        Ok(())
    }

    async fn call(&self, req: Request, body: &[u8]) -> Result<(Response, Vec<u8>)> {
        let (mut send, mut recv) = self.conn.open_bi().await?;
        let env = Envelope { grant: self.grant(), req };
        send.write_all(&encode_frame(&env, body)?).await?;
        send.finish()?;
        decode_frame(&recv.read_to_end(MAX_FRAME).await?)
    }

    /// Present the invite; returns this client's member id.
    pub async fn join(&self, invite: &str) -> Result<String> {
        match self.call(Request::Join { invite: invite.to_string() }, &[]).await?.0 {
            Response::Joined { member, grant } => {
                self.set_grant(grant);
                Ok(member)
            }
            Response::Error { message } => bail!("join refused: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub async fn echo(&self, text: &str) -> Result<String> {
        match self.call(Request::Echo { text: text.to_string() }, &[]).await?.0 {
            Response::Echo { text, .. } => Ok(text),
            Response::Error { message } => bail!("echo refused: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Fetch `refname` from the room; returns its commit id and a git bundle.
    pub async fn pull(&self, refname: &str) -> Result<(String, Vec<u8>)> {
        match self.call(Request::Pull { refname: refname.to_string() }, &[]).await? {
            (Response::Pulled { head }, bundle) => Ok((head, bundle)),
            (Response::Error { message }, _) => bail!("pull refused: {message}"),
            (other, _) => bail!("unexpected response: {other:?}"),
        }
    }

    /// Send a bundle holding `branch`. Returns the ref it was stored under on the host.
    pub async fn push(&self, branch: &str, bundle: &[u8]) -> Result<String> {
        match self.call(Request::Push { branch: branch.to_string() }, bundle).await?.0 {
            Response::Pushed { reference, .. } => Ok(reference),
            Response::Error { message } => bail!("push refused: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Run a task on a checkout of `base`; returns the runner name, its output and the patch.
    pub async fn run_task(&self, base: &str, spec: TaskSpec) -> Result<(String, TaskOutput, Vec<u8>)> {
        match self.call(Request::RunTask { base: base.to_string(), spec }, &[]).await? {
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
/// `transport` may be a [`Transport`], or a bool where `true` forces local and `false` follows the link.
pub async fn join_link(link: &str, secret: SecretKey, transport: impl Into<TransportChoice>) -> Result<Client> {
    let link = Link::parse(link)?;
    let transport = transport.into().resolve(link.transport.as_ref());
    let client = Client::connect(link.addr, secret, transport).await?;
    client.join(&link.invite).await?;
    Ok(client)
}
