//! Playbox networking: a host that admits members by invite and a client that joins by link.
//!
//! Peers speak A2A v1 JSON-RPC over iroh streams; see [`wire`] for the binding.

pub mod audit;
pub mod grants;
pub mod limits;
pub mod review;
pub mod revoke;
pub mod steer;
pub mod transport;
pub mod wire;
pub use revoke::{Notice, SYNC_ALPN};
pub use transport::{Transport, TransportChoice};

use anyhow::{anyhow, bail, Context, Result};
use grants::{Authority, Caps};
pub use iroh::EndpointId;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointAddr, SecretKey,
};
use a2a::{
    A2AError, GetTaskRequest, JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse, ListTasksResponse, Message,
    Part, Role, SendMessageRequest, SendMessageResponse, Task, TaskState,
};
use ed25519_dalek::SigningKey;
use playbox_core::attest::{self, Measurements, PlatformProvider, PlatformVerifier, Tier, Verified};
use playbox_runner::{Isolation, Limits, Runner, TaskOutput, TaskSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wire::Op;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Semaphore;

pub const ALPN: &[u8] = b"playbox/a2a/1";
/// Cap for one request or response. Bundles and patches travel base64-encoded inside it, so the
/// largest usable payload is about three quarters of this.
const MAX_FRAME: usize = 256 * 1024 * 1024;
/// The most a host will read from a connection that has not yet proved anything: the first request on
/// a connection is at most this big. After one request succeeds with an invite or a grant, the connection
/// may send up to `MAX_FRAME`. Clients with a large first request send a tiny one first (see `Client`).
pub const MAX_UNAUTHENTICATED_REQUEST: usize = 64 * 1024;
/// How long a connection that has proved something may sit idle, or take to send one request.
const AUTHENTICATED_PATIENCE: Duration = Duration::from_secs(15 * 60);
/// Tasks remembered per member for `GetTask` / `ListTasks`.
const TASKS_KEPT: usize = 200;
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
    /// The invite works only for the key the host was told about (see [`LinkOpts::bind_to`]). The guest
    /// then joins with its stable identity key instead of a fresh one.
    pub bound: bool,
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

/// Where a guest's stable identity key lives, for links bound to it.
pub fn identity_key(path: &std::path::Path) -> Result<SecretKey> {
    match std::fs::read(path) {
        Ok(b) => Ok(SecretKey::from_bytes(&<[u8; 32]>::try_from(b.as_slice()).map_err(|_| anyhow!("{} is not a 32-byte key", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let k = random_bytes()?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, k)?;
            #[cfg(unix)]
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
            Ok(SecretKey::from_bytes(&k))
        }
        Err(e) => Err(e).context("reading identity key"),
    }
}

/// The key to join `link` with: a fresh one, or `identity` when the link is bound to a key.
pub fn key_for_link(link: &str, identity: &std::path::Path) -> Result<SecretKey> {
    if Link::parse(link)?.bound { identity_key(identity) } else { new_secret_key() }
}

/// A host's address as one copy-pasteable string, for `--sync-peer`.
pub fn encode_peer(addr: &EndpointAddr) -> String {
    B64.encode(serde_json::to_vec(addr).expect("EndpointAddr serializes"))
}

/// Either a full address from [`encode_peer`], or just an endpoint id (64 hex characters). An id alone
/// lets that host pull notices from us and join the gossip, but we cannot dial it until it contacts us.
pub fn decode_peer(s: &str) -> Result<EndpointAddr> {
    let s = s.trim();
    if let Ok(id) = s.parse::<EndpointId>() {
        return Ok(EndpointAddr::new(id));
    }
    let json = B64.decode(s).context("peer is neither an endpoint id nor an encoded address")?;
    serde_json::from_slice(&json).context("peer address is not valid")
}

#[derive(Serialize, Deserialize)]
struct LinkBody {
    addr: EndpointAddr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    t: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    b: bool,
}

impl Link {
    pub fn encode(&self) -> String {
        let body = LinkBody { addr: self.addr.clone(), t: self.transport.as_ref().map(Transport::label), b: self.bound };
        let json = serde_json::to_vec(&body).expect("link body serializes");
        format!("{LINK_SCHEME}{}#{}", B64.encode(json), self.invite)
    }

    pub fn parse(s: &str) -> Result<Self> {
        let rest = s.strip_prefix(LINK_SCHEME).ok_or_else(|| anyhow!("link must start with {LINK_SCHEME}"))?;
        let (body, invite) = rest.split_once('#').ok_or_else(|| anyhow!("link has no invite fragment"))?;
        let json = B64.decode(body).context("link is not valid base64")?;
        let (addr, t, bound) = match serde_json::from_slice::<LinkBody>(&json) {
            Ok(b) => (b.addr, b.t, b.b),
            Err(_) => (serde_json::from_slice(&json).context("link address is not valid")?, None, false),
        };
        if invite.is_empty() {
            bail!("empty invite");
        }
        let transport = t.map(|t| Transport::parse(&t)).transpose()?;
        Ok(Self { addr, invite: invite.to_string(), transport, bound })
    }
}

#[derive(Debug)]
struct Invite {
    uses_left: u32,
    /// If set, only this key may redeem the invite.
    bound_to: Option<EndpointId>,
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
    /// Signed revocation notices by grant id: the room's revocations, kept so they can be passed on.
    notices: BTreeMap<String, String>,
    /// Notices read from disk, not yet verified. `build` verifies and applies them.
    pending: Vec<String>,
    /// Where this state is saved after every change, if the host keeps state on disk.
    path: Option<PathBuf>,
}

/// On-disk form of [`State`].
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    invites: HashMap<String, u32>,
    /// Invite id -> the only key allowed to redeem it.
    #[serde(default)]
    bound: HashMap<String, String>,
    members: HashMap<String, String>,
    revoked: Vec<String>,
    banned: Vec<String>,
    #[serde(default)]
    notices: Vec<String>,
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
        for (k, v) in saved.invites {
            let bound_to = saved.bound.get(&k).map(|b| id(b)).transpose()?;
            st.invites.insert(k, Invite { uses_left: v, bound_to });
        }
        for (m, g) in saved.members {
            st.members.insert(id(&m)?, g);
        }
        st.revoked = saved.revoked.into_iter().collect();
        for b in saved.banned {
            st.banned.insert(id(&b)?);
        }
        st.pending = saved.notices;
        Ok(st)
    }

    /// Write the state next to its final path, then rename, so a crash never leaves half a file.
    /// A failure is logged, not fatal: the room keeps working but the change may not survive a restart.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let saved = Saved {
            invites: self.invites.iter().map(|(k, v)| (k.clone(), v.uses_left)).collect(),
            bound: self.invites.iter().filter_map(|(k, v)| v.bound_to.map(|b| (k.clone(), b.to_string()))).collect(),
            members: self.members.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            revoked: self.revoked.iter().cloned().collect(),
            banned: self.banned.iter().map(ToString::to_string).collect(),
            notices: self.notices.values().cloned().collect(),
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
#[derive(Clone)]
pub struct HostConfig {
    /// Weakest isolation this host accepts from its runner. The host refuses to start below it,
    /// so a missing sandbox never turns into unsandboxed execution. Defaults to `Container`.
    pub min_isolation: Isolation,
    /// Most simultaneous connections the host serves; the rest are turned away. Default 256.
    pub max_connections: usize,
    /// Most simultaneous connections from one identity. Default 8. Stops a single member (or a stranger
    /// with one key) from using every slot.
    pub max_connections_per_peer: usize,
    /// Most git data one member may push in total since the host started (not saved across restarts).
    /// Default 1 GiB. Pushed objects are stored in the room's repository and never deleted automatically.
    pub max_push_bytes: u64,
    /// How much a task may write while it runs (growth of its working directory). A task past it is stopped
    /// and its sandbox killed. Default 1 GiB and 100 000 entries.
    pub scratch_quota: playbox_runner::Quota,
    /// Where the tamper-evident audit log goes. `None`: `<state dir>/audit.log` for a host opened with a state
    /// directory, and no log for an ephemeral host. The path `-` means standard error.
    pub audit_log: Option<PathBuf>,
    /// How long a connection that has not authenticated may take to send its first request, or to send
    /// anything at all. Default 30 s.
    pub idle_timeout: Duration,
    /// The room's git repository (a normal or bare repo).
    pub repo: Option<PathBuf>,
    /// Where member tasks run. Without one, `RunTask` is refused.
    pub runner: Option<Arc<dyn Runner>>,
    /// Limits written into every member grant. A task asking for more is refused.
    pub limits: Limits,
    /// How long member grants last. Defaults to one hour.
    pub grant_ttl: Option<Duration>,
    /// Versioned workspaces. When set, `RunTask` runs against the workspace branch named by `base`:
    /// each member gets a branch `members/<id>` forked from it, and every task becomes one commit
    /// there (readable through this handle). Tasks serialise on this lock. Takes precedence over `repo`.
    pub workspace: Option<Arc<tokio::sync::Mutex<playbox_core::workspace::Repo>>>,
    /// Hardware evidence (a TPM quote, a Secure Enclave signature) to attach to attestations. Without
    /// one, attestations are self-signed and achieve at most T0. If the provider fails at request
    /// time the host logs it and answers with a plain statement rather than breaking the room.
    pub platform: Option<Arc<dyn PlatformProvider>>,
    /// Steering: the weights this host serves, the engine image, and what members are issued. Needs
    /// `runner`. See `docs/steering.md`.
    pub steer: Option<Arc<steer::SteerHost>>,
    /// Member cap, per-member concurrency and rate limits, and a total task budget.
    pub abuse: limits::AbuseLimits,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            min_isolation: Isolation::Container,
            max_connections: 256,
            max_connections_per_peer: 8,
            max_push_bytes: 1 << 30,
            scratch_quota: playbox_runner::Quota::default(),
            audit_log: None,
            idle_timeout: Duration::from_secs(30),
            repo: None,
            runner: None,
            limits: Limits::default(),
            grant_ttl: None,
            workspace: None,
            platform: None,
            steer: None,
            abuse: limits::AbuseLimits::default(),
        }
    }
}

#[derive(Clone)]
struct RoomHandler {
    state: Arc<Mutex<State>>,
    cfg: HostConfig,
    tasks: Arc<Semaphore>,
    limiter: limits::Limiter,
    conns: Arc<Semaphore>,
    audit: Arc<audit::Audit>,
    peer_conns: Arc<Mutex<HashMap<EndpointId, usize>>>,
    pushed: Arc<Mutex<HashMap<EndpointId, u64>>>,
    /// Bounds concurrent calls into platform hardware, which anyone may trigger without a grant.
    platform_calls: Arc<Semaphore>,
    auth: Arc<Authority>,
    /// Recent tasks per member, with byte payloads dropped. Not persisted.
    done: Arc<Mutex<HashMap<EndpointId, VecDeque<Task>>>>,
    card: Arc<Value>,
    /// Signs attestation statements with the host's endpoint key, so a client can bind them to the
    /// connection it already authenticated.
    attest_key: Arc<SigningKey>,
    measurements: Arc<Measurements>,
    platform: Option<Arc<dyn PlatformProvider>>,
}

impl std::fmt::Debug for RoomHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomHandler")
    }
}

/// Removes a task's scratch checkout even when the task is cancelled (its future dropped) mid-run.
struct WorktreeGuard {
    repo: PathBuf,
    wt: Option<playbox_git::Worktree>,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if let (Some(wt), Ok(h)) = (self.wt.take(), tokio::runtime::Handle::try_current()) {
            let repo = self.repo.clone();
            h.spawn(async move {
                let _ = playbox_git::worktree_remove(&repo, &wt).await;
            });
        }
    }
}

/// An error that means "this room will not do that" (a Rejected task), as opposed to "it tried and failed".
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refuse(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Refused(msg.into()))
}

fn rpc_error(code: i32, message: impl Into<String>) -> JsonRpcError {
    JsonRpcError { code, message: message.into(), data: None }
}

fn unauthorized(e: anyhow::Error) -> JsonRpcError {
    rpc_error(wire::ERR_UNAUTHORIZED, format!("{e:#}"))
}

/// A task for a result: success, or Rejected / Failed according to the error.
fn finish(result: Result<Task>) -> Task {
    match result {
        Ok(t) => t,
        Err(e) => {
            let state = if e.downcast_ref::<Refused>().is_some() { TaskState::Rejected } else { TaskState::Failed };
            wire::task(state, Some(format!("{e:#}")), vec![], vec![])
        }
    }
}

impl RoomHandler {
    fn join(&self, from: EndpointId, invite: String) -> Result<Value> {
        if !self.auth.can_issue() {
            bail!("this host is a replica and cannot admit members; join through the room's primary host");
        }
        if self.state.lock().expect("state lock").banned.contains(&from) {
            bail!("this key is banned from the room");
        }
        let id = self.auth.verify_invite(&invite)?;
        let mut st = self.state.lock().expect("state lock");
        if st.members.len() >= self.limiter.max_members() && !st.members.contains_key(&from) {
            bail!("this room is full ({} members)", self.limiter.max_members());
        }
        let inv = st.invites.get_mut(&id).ok_or_else(|| anyhow!("invite already used or unknown"))?;
        if inv.bound_to.is_some_and(|b| b != from) {
            // Checked before the invite is spent, so a stranger holding a leaked link cannot burn it.
            bail!("this invite is for a different key");
        }
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
            hosts: l.allowed_hosts.clone(),
        };
        let ttl = self.cfg.grant_ttl.unwrap_or(Duration::from_secs(3600)).as_secs() as i64;
        let (grant, grant_id) = self.auth.mint_grant_with(&from.to_string(), ttl, &caps, self.cfg.steer.as_ref().map(|h| h.caps()))?;
        st.members.insert(from, grant_id);
        st.save();
        Ok(json!({ "member": from.to_string(), "grant": grant }))
    }

    fn authorize(&self, grant: Option<&str>, from: EndpointId, op: &str, needs: &grants::Needs) -> Result<(), JsonRpcError> {
        let Some(grant) = grant else { return Err(unauthorized(anyhow!("no grant presented"))) };
        let revoked = self.state.lock().expect("state lock").revoked.clone();
        self.auth.authorize(grant, &from.to_string(), op, needs, &revoked).map_err(unauthorized)
    }

    /// One JSON-RPC call from `from`.
    async fn route(&self, from: EndpointId, frame: &wire::Frame) -> Result<Value, JsonRpcError> {
        use a2a::methods;
        let rpc = &frame.rpc;
        let params = rpc.params.clone().unwrap_or(Value::Null);
        let invalid = |e: serde_json::Error| A2AError::invalid_params(e.to_string()).to_jsonrpc_error();
        match rpc.method.as_str() {
            wire::METHOD_AGENT_CARD => Ok((*self.card).clone()),
            wire::METHOD_ACTIVATE => {
                let Some(p) = self.platform.clone() else {
                    return Err(A2AError::unsupported_operation("this host has no platform hardware").to_jsonrpc_error());
                };
                let req = params.get("request").and_then(Value::as_str).unwrap_or_default();
                if req.len() > 16384 || req.len() % 2 != 0 || !req.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(A2AError::invalid_params("`request` must be at most 8 KiB of hex").to_jsonrpc_error());
                }
                let bytes: Vec<u8> = (0..req.len() / 2).map(|i| u8::from_str_radix(&req[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect();
                let _slot = self
                    .platform_calls
                    .try_acquire()
                    .map_err(|_| A2AError::unsupported_operation("busy: too many hardware challenges in flight, try again").to_jsonrpc_error())?;
                let out = tokio::task::spawn_blocking(move || p.respond(&bytes)).await.map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())?;
                match out {
                    Ok(r) => Ok(json!({ "response": r.iter().map(|b| format!("{b:02x}")).collect::<String>() })),
                    Err(e) => Err(rpc_error(wire::ERR_UNAUTHORIZED, format!("challenge failed: {e}"))),
                }
            }
            wire::METHOD_ATTESTATION => {
                let nonce = params.get("nonce").and_then(Value::as_str).unwrap_or_default();
                if nonce.len() > 64 || !nonce.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(A2AError::invalid_params("`nonce` must be at most 64 hex characters").to_jsonrpc_error());
                }
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
                let plain = || {
                    attest::new_statement(&self.attest_key, nonce, "", now, (*self.measurements).clone())
                        .sign(&self.attest_key)
                        .map_err(|e| A2AError::internal(e).to_jsonrpc_error())
                };
                let text = match &self.platform {
                    None => plain()?,
                    Some(p) => {
                        // Anyone may ask for this without a grant and each one drives the hardware, so bound
                        // how many run at once and tell the rest to come back.
                        let _slot = self
                            .platform_calls
                            .try_acquire()
                            .map_err(|_| A2AError::unsupported_operation("busy: too many attestations in flight, try again").to_jsonrpc_error())?;
                        // Talking to hardware blocks (a TPM quote takes a moment), so keep it off the runtime threads.
                        let (key, m, p, nonce) = (self.attest_key.clone(), (*self.measurements).clone(), p.clone(), nonce.to_string());
                        let made = tokio::task::spawn_blocking(move || attest::new_platform_statement(&key, &nonce, "", now, m, p.as_ref())).await;
                        match made {
                            Ok(Ok(t)) => t,
                            Ok(Err(e)) => {
                                eprintln!("playbox: platform attestation failed, answering with a plain statement: {e}");
                                plain()?
                            }
                            Err(e) => {
                                eprintln!("playbox: platform attestation task failed: {e}");
                                plain()?
                            }
                        }
                    }
                };
                Ok(json!({ "statement": text }))
            }
            wire::METHOD_JOIN => {
                let invite = params.get("invite").and_then(Value::as_str).ok_or_else(|| {
                    A2AError::invalid_params("missing `invite`").to_jsonrpc_error()
                })?;
                let joined = self.join(from, invite.to_string());
                self.audit.record(
                    "join",
                    Some(&from.to_string()),
                    json!({ "ok": joined.is_ok(), "reason": joined.as_ref().err().map(|e| audit::clip(&format!("{e:#}"))) }),
                );
                joined.map_err(unauthorized)
            }
            methods::SEND_MESSAGE => {
                let req: SendMessageRequest = serde_json::from_value(params).map_err(invalid)?;
                let op = Op::from_message(&req.message).map_err(|e| A2AError::invalid_params(format!("{e:#}")).to_jsonrpc_error())?;
                // A plain A2A message with no Playbox operation only needs membership, like echo.
                let (name, needs) = match &op {
                    // Membership is checked here; the steering rights and limits need the spec and the
                    // prompt set, so `steer_message` authorizes them before it does any work.
                    Some(Op::Steer { .. }) => ("echo", grants::Needs::default()),
                    Some(op @ Op::Handoff { .. }) => (op.name(), grants::Needs::default()),
                    Some(op) => (op.name(), op.needs()),
                    None => ("echo", grants::Needs::default()),
                };
                self.authorize(frame.grant(), from, name, &needs)?;
                if matches!(op, Some(Op::Steer { .. })) {
                    return self.steer_message(from, frame.grant(), op.expect("matched Some"), &req.message).await;
                }
                if let Some(op @ Op::Handoff { .. }) = op {
                    return self.handoff_message(from, op, &req.message).await;
                }
                let reply = self.send_message(from, op, &req.message).await;
                serde_json::to_value(reply).map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())
            }
            methods::GET_TASK => {
                self.authorize(frame.grant(), from, "tasks", &grants::Needs::default())?;
                let req: GetTaskRequest = serde_json::from_value(params).map_err(invalid)?;
                let t = self.find_task(from, &req.id).ok_or_else(|| A2AError::task_not_found(&req.id).to_jsonrpc_error())?;
                serde_json::to_value(t).map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())
            }
            methods::LIST_TASKS => {
                self.authorize(frame.grant(), from, "tasks", &grants::Needs::default())?;
                let tasks: Vec<Task> = self.done.lock().expect("tasks lock").get(&from).map(|q| q.iter().cloned().collect()).unwrap_or_default();
                let n = tasks.len() as i32;
                let resp = ListTasksResponse { tasks, next_page_token: String::new(), page_size: n, total_size: n };
                serde_json::to_value(resp).map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())
            }
            methods::CANCEL_TASK => {
                self.authorize(frame.grant(), from, "tasks", &grants::Needs::default())?;
                let id = params.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                // Every task here finishes before its response is sent, so none can still be cancelled.
                match self.find_task(from, &id) {
                    Some(_) => Err(A2AError::task_not_cancelable(&id).to_jsonrpc_error()),
                    None => Err(A2AError::task_not_found(&id).to_jsonrpc_error()),
                }
            }
            m if methods::is_streaming(m) => Err(A2AError::unsupported_operation("streaming is not supported by this room").to_jsonrpc_error()),
            methods::GET_EXTENDED_AGENT_CARD => Err(A2AError::extended_card_not_configured().to_jsonrpc_error()),
            m if methods::is_valid(m) => Err(A2AError::unsupported_operation(format!("{m} is not supported by this room")).to_jsonrpc_error()),
            m => Err(rpc_error(a2a::error_code::METHOD_NOT_FOUND, format!("method not found: {m}"))),
        }
    }

    fn find_task(&self, from: EndpointId, id: &str) -> Option<Task> {
        self.done.lock().expect("tasks lock").get(&from)?.iter().find(|t| t.id == id).cloned()
    }

    fn remember(&self, from: EndpointId, t: &Task) {
        let mut done = self.done.lock().expect("tasks lock");
        let q = done.entry(from).or_default();
        q.push_back(wire::without_bytes(t.clone()));
        while q.len() > TASKS_KEPT {
            q.pop_front();
        }
    }

    async fn send_message(&self, from: EndpointId, op: Option<Op>, msg: &Message) -> SendMessageResponse {
        let reply = |text: String| SendMessageResponse::Message(Message::new(Role::Agent, vec![Part::text(text)]));
        let task = match op {
            None => {
                return reply(
                    "Playbox room. Send a data part like {\"playbox\":{\"op\":\"echo\",\"text\":\"hi\"}}; \
                     see the skills on the agent card (playbox.GetAgentCard)."
                        .into(),
                )
            }
            Some(Op::Echo { text }) => return reply(text),
            // Handled in `route`, which has the grant it needs to authorize the specifics.
            Some(Op::Steer { .. } | Op::Handoff { .. }) => return reply("steering is handled by the router".into()),
            Some(Op::Pull { refname }) => {
                let t0 = std::time::Instant::now();
                let r = self.pull(&refname).await;
                self.audit_op(from, "pull", &r, t0, json!({ "ref": audit::clip(&refname) }));
                finish(r)
            }
            Some(Op::Push { branch }) => {
                let t0 = std::time::Instant::now();
                let bytes = wire::raw_part(msg).map(<[u8]>::len);
                let r = self.push(from, &branch, wire::raw_part(msg)).await;
                self.audit_op(from, "push", &r, t0, json!({ "branch": audit::clip(&branch), "bytes": bytes }));
                finish(r)
            }
            Some(Op::Run { base, spec }) => {
                let t0 = std::time::Instant::now();
                // Arguments often hold secrets, so the log keeps the program, how many arguments, and a hash.
                let mut h = blake3::Hasher::new();
                for a in &spec.cmd {
                    h.update(a.as_bytes());
                    h.update(&[0]);
                }
                let details = json!({
                    "base": audit::clip(&base),
                    "program": spec.cmd.first().map(|c| audit::clip(c)),
                    "args": spec.cmd.len().saturating_sub(1),
                    "cmd_hash": h.finalize().to_hex().chars().take(16).collect::<String>(),
                    "image": audit::clip(&spec.image),
                    "cpus": spec.cpus, "mem_mib": spec.mem_mib, "timeout_secs": spec.timeout_secs, "network": spec.network,
                });
                let r = self.run(from, &base, spec).await;
                self.audit_op(from, "task", &r, t0, details);
                finish(r)
            }
        };
        self.remember(from, &task);
        SendMessageResponse::Task(task)
    }

    /// Record how a pull, push or task went, from the task it produced or the error that stopped it.
    fn audit_op(&self, from: EndpointId, event: &str, result: &Result<Task>, started: std::time::Instant, mut details: Value) {
        let Some(obj) = details.as_object_mut() else { return };
        obj.insert("ms".into(), json!(started.elapsed().as_millis() as u64));
        match result {
            Ok(t) => {
                obj.insert("ok".into(), json!(true));
                obj.insert("state".into(), json!(format!("{:?}", t.status.state)));
                for (key, out) in [("exitCode", "exit_code"), ("timedOut", "timed_out"), ("runner", "runner"), ("commit", "commit"), ("reference", "reference")] {
                    if let Some(v) = wire::meta(t, key) {
                        obj.insert(out.into(), v.clone());
                    }
                }
            }
            Err(e) => {
                obj.insert("ok".into(), json!(false));
                obj.insert("reason".into(), json!(audit::clip(&format!("{e:#}"))));
            }
        }
        self.audit.record(event, Some(&from.to_string()), details);
    }

    fn repo(&self) -> Result<PathBuf> {
        self.cfg.repo.clone().ok_or_else(|| refuse("this room has no git repository"))
    }

    async fn pull(&self, refname: &str) -> Result<Task> {
        let repo = self.repo()?;
        playbox_git::check_name(refname).map_err(|e| refuse(format!("{e:#}")))?;
        let head = playbox_git::rev_parse(&repo, refname).await?;
        let bundle = playbox_git::bundle_create(&repo, refname).await?;
        Ok(wire::task(
            TaskState::Completed,
            None,
            vec![wire::raw_artifact("bundle", "ref.bundle", wire::MEDIA_BUNDLE, bundle)],
            vec![("head", json!(head))],
        ))
    }

    async fn push(&self, from: EndpointId, branch: &str, bundle: Option<&[u8]>) -> Result<Task> {
        let repo = self.repo()?;
        let branch = playbox_git::check_name(branch).map_err(|e| refuse(format!("{e:#}")))?;
        let bundle = bundle.ok_or_else(|| refuse("push needs the git bundle as a raw part of the message"))?;
        {
            let mut pushed = self.pushed.lock().expect("pushed lock");
            let used = pushed.entry(from).or_default();
            if used.saturating_add(bundle.len() as u64) > self.cfg.max_push_bytes {
                return Err(refuse(format!(
                    "push quota exceeded: this host accepts {} MiB per member and you have used {} MiB",
                    self.cfg.max_push_bytes >> 20,
                    *used >> 20
                )));
            }
            *used += bundle.len() as u64; // counted on attempt, so failing pushes cannot be repeated for free
        }
        let short: String = from.to_string().chars().take(16).collect();
        let reference = format!("refs/playbox/members/{short}/{branch}");
        let head = playbox_git::bundle_import(&repo, bundle, &format!("refs/heads/{branch}"), &reference).await?;
        Ok(wire::task(TaskState::Completed, None, vec![], vec![("reference", json!(reference)), ("head", json!(head))]))
    }

    /// Run a member's task under the abuse limits, and stop it if the member is revoked while it runs.
    /// Stopping drops the run, which kills its sandbox.
    async fn run(&self, from: EndpointId, base: &str, spec: TaskSpec) -> Result<Task> {
        let _admission = self.limiter.admit(from).map_err(refuse)?;
        tokio::select! {
            r = self.run_inner(from, base, spec) => r,
            () = self.until_revoked(from) => {
                self.audit.record("task_cancelled", Some(&from.to_string()), json!({ "reason": "member revoked" }));
                Err(refuse("cancelled: your access to this room was revoked"))
            }
        }
    }

    /// Resolves once `who` has been revoked (here or by a notice that reached this host).
    async fn until_revoked(&self, who: EndpointId) {
        loop {
            if self.state.lock().expect("state lock").banned.contains(&who) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn run_inner(&self, from: EndpointId, base: &str, spec: TaskSpec) -> Result<Task> {
        let runner = self.cfg.runner.clone().ok_or_else(|| refuse("this room runs no tasks"))?;
        let (output, patch, extra) = if let Some(ws) = &self.cfg.workspace {
            let _permit = self.tasks.acquire().await?;
            let short: String = from.to_string().chars().take(16).collect();
            let branch = format!("members/{short}");
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
            let mut repo = ws.lock().await;
            if repo.head(&branch).is_err() {
                repo.fork(&branch, base)?;
            }
            let mut wsp = repo.workspace(&branch)?;
            let run = playbox_runner::workspace::run_in_workspace(&mut repo, &mut wsp, runner.as_ref(), &spec, &from.to_string(), &format!("rpc-{now}"), now, self.cfg.scratch_quota).await?;
            if !run.conflicts.is_empty() {
                bail!("conflict on {:?}", run.conflicts);
            }
            // The commit stays on the host (branch `members/<id>`); the patch is empty. The commit id
            // and the changed paths come back as task metadata.
            let mut extra = vec![("branch", json!(branch)), ("changed", json!(run.changed))];
            if let Some(c) = run.commit {
                extra.push(("commit", json!(playbox_core::history::text::hex(&c))));
            }
            (run.output, Vec::new(), extra)
        } else {
            let repo = self.repo()?;
            playbox_git::check_name(base).map_err(|e| refuse(format!("{e:#}")))?;
            let _permit = self.tasks.acquire().await?;
            let commit = playbox_git::rev_parse(&repo, base).await?;
            let mut guard = WorktreeGuard { repo: repo.clone(), wt: Some(playbox_git::worktree_add(&repo, &commit).await?) };
            let result = async {
                let wt = guard.wt.as_ref().expect("worktree present");
                let output = playbox_runner::run_with_quota(runner.as_ref(), &spec, wt.path(), self.cfg.scratch_quota).await?;
                let patch = playbox_git::diff_patch(wt, &commit).await?;
                anyhow::Ok((output, patch))
            }
            .await;
            if let Some(wt) = guard.wt.take() {
                let _ = playbox_git::worktree_remove(&repo, &wt).await;
            }
            let (output, patch) = result?;
            (output, patch, Vec::new())
        };
        let ok = output.exit_code == Some(0);
        let text = match (ok, output.timed_out, output.exit_code) {
            (true, _, _) => None,
            (_, true, _) => Some("timed out".to_string()),
            (_, _, Some(c)) => Some(format!("exit code {c}")),
            _ => Some("terminated by a signal".to_string()),
        };
        let mut artifacts = vec![wire::text_artifact("stdout", output.stdout), wire::text_artifact("stderr", output.stderr)];
        if !patch.is_empty() {
            artifacts.push(wire::raw_artifact("patch", "changes.patch", wire::MEDIA_PATCH, patch));
        }
        let mut meta = vec![("runner", json!(runner.name())), ("timedOut", json!(output.timed_out))];
        if let Some(d) = &output.policy_digest {
            meta.push(("policyDigest", json!(d)));
        }
        if let Some(c) = output.exit_code {
            meta.push(("exitCode", json!(c)));
        }
        meta.extend(extra);
        Ok(wire::task(if ok { TaskState::Completed } else { TaskState::Failed }, text, artifacts, meta))
    }

    /// Answer one request. The flag says whether the caller proved itself (a valid invite or grant),
    /// which is what lets its connection send large requests afterwards.
    async fn dispatch(&self, from: EndpointId, bytes: &[u8]) -> (JsonRpcResponse, bool) {
        let frame: wire::Frame = match serde_json::from_slice(bytes) {
            Ok(f) => f,
            Err(e) => return (JsonRpcResponse::error(JsonRpcId::Null, A2AError::parse_error(e.to_string()).to_jsonrpc_error()), false),
        };
        let id = frame.rpc.id.clone();
        let public = wire::is_public(&frame.rpc.method);
        match self.route(from, &frame).await {
            Ok(v) => (JsonRpcResponse::success(id, v), !public),
            Err(e) => {
                if !public {
                    // Denials and failures of anything that needed a grant or invite. The text is clipped and
                    // contains no token: Biscuit errors name the failed check, not the token.
                    self.audit.record(
                        "refused",
                        Some(&from.to_string()),
                        json!({ "method": audit::clip(&frame.rpc.method), "code": e.code, "reason": audit::clip(&e.message) }),
                    );
                }
                (JsonRpcResponse::error(id, e), false)
            }
        }
    }
}

/// Counts one connection against its peer and gives the slot back when dropped.
struct PeerSlot {
    map: Arc<Mutex<HashMap<EndpointId, usize>>>,
    id: EndpointId,
}

impl PeerSlot {
    fn take(map: &Arc<Mutex<HashMap<EndpointId, usize>>>, id: EndpointId, max: usize) -> Option<PeerSlot> {
        let mut m = map.lock().expect("peer lock");
        let n = m.entry(id).or_default();
        if *n >= max.max(1) {
            return None;
        }
        *n += 1;
        Some(PeerSlot { map: map.clone(), id })
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut m = self.map.lock().expect("peer lock");
        if let Some(n) = m.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.id);
            }
        }
    }
}

impl ProtocolHandler for RoomHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // Anyone with an iroh identity can dial this, so everything before authentication is bounded:
        // how many connections, how long they may stay silent, and how much they may make us read.
        let from = conn.remote_id();
        let who = from.to_string();
        let Ok(_slot) = self.conns.clone().try_acquire_owned() else {
            self.audit.record("conn_refused", Some(&who), json!({ "why": "host is at its connection limit" }));
            conn.close(2u32.into(), b"too many connections");
            return Ok(());
        };
        let Some(_peer_slot) = PeerSlot::take(&self.peer_conns, from, self.cfg.max_connections_per_peer) else {
            self.audit.record("conn_refused", Some(&who), json!({ "why": "too many connections from this identity" }));
            conn.close(2u32.into(), b"too many connections from this identity");
            return Ok(());
        };
        let mut authenticated = false;
        loop {
            let (patience, cap) = if authenticated {
                (AUTHENTICATED_PATIENCE, MAX_FRAME)
            } else {
                (self.cfg.idle_timeout, MAX_UNAUTHENTICATED_REQUEST)
            };
            let Ok(Ok((mut send, mut recv))) = tokio::time::timeout(patience, conn.accept_bi()).await else { break };
            let Ok(Ok(bytes)) = tokio::time::timeout(patience, recv.read_to_end(cap)).await else {
                self.audit.record("request_dropped", Some(&who), json!({ "why": "too large or too slow", "limit_bytes": cap, "authenticated": authenticated }));
                conn.close(1u32.into(), b"request too large or too slow");
                return Ok(());
            };
            let (resp, proved) = self.dispatch(from, &bytes).await;
            authenticated |= proved;
            let Ok(out) = serde_json::to_vec(&resp) else { break };
            if send.write_all(&out).await.is_err() || send.finish().is_err() {
                break;
            }
        }
        // Reached when the peer hung up, a write failed, or the connection sat idle past its patience.
        // Any response was sent before the wait began, so closing now loses nothing; and waiting for
        // the peer to close would keep a silent connection (and its slot) alive forever.
        conn.close(0u32.into(), b"done");
        Ok(())
    }
}

async fn bind(secret: SecretKey, transport: &Transport) -> Result<(Endpoint, iroh::address_lookup::memory::MemoryLookup)> {
    // The in-memory lookup is how a host learns the addresses of its sync peers.
    let memory = iroh::address_lookup::memory::MemoryLookup::new();
    let b = transport.configure(transport.builder())?;
    let ep = b.secret_key(secret).alpns(vec![ALPN.to_vec()]).address_lookup(memory.clone()).bind().await?;
    Ok((ep, memory))
}

/// Where a host's authority comes from.
enum Root {
    /// A new random root key (not persisted).
    Fresh,
    /// A stored root key.
    Seed([u8; 32]),
    /// Only the room's public key: a replica.
    Verifier([u8; 32]),
}

pub struct Host {
    audit: Arc<audit::Audit>,
    router: Router,
    state: Arc<Mutex<State>>,
    auth: Arc<Authority>,
    transport: Transport,
    repl: Arc<revoke::Replication>,
}

impl Host {
    /// `transport` may be a [`Transport`] or a bool (`true` = local, `false` = public iroh).
    pub async fn start(secret: SecretKey, transport: impl Into<Transport>) -> Result<Self> {
        Self::start_with(secret, transport, HostConfig::default()).await
    }

    pub async fn start_with(secret: SecretKey, transport: impl Into<Transport>, cfg: HostConfig) -> Result<Self> {
        Self::build(secret, transport.into(), cfg, Root::Fresh, State::default()).await
    }

    /// Start a host that keeps its identity, signing key, invites, members and bans in `dir`.
    /// Restarting with the same `dir` gives the same endpoint id, keeps issued grants valid, and keeps
    /// revoked members out. Links created before the restart still work if their address does.
    pub async fn open(dir: &std::path::Path, transport: impl Into<Transport>, cfg: HostConfig) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let secret = SecretKey::from_bytes(&load_or_create_seed(&dir.join("endpoint.key"))?);
        let root_seed = load_or_create_seed(&dir.join("root.key"))?;
        let state = State::load(&dir.join("state.json"))?;
        let mut cfg = cfg;
        cfg.audit_log.get_or_insert_with(|| dir.join("audit.log"));
        Self::build(secret, transport.into(), cfg, Root::Seed(root_seed), state).await
    }

    /// Start a **replica**: another host of an existing room. It knows the room's root *public* key,
    /// so it accepts the room's grants and checks its revocations, but it holds no signing key: it
    /// cannot admit members, issue grants, or revoke. Give it the same `--sync-peer`s as the primary
    /// (see [`Host::enable_revocation_sync`]) so revocations reach it.
    pub async fn open_replica(
        dir: &std::path::Path,
        transport: impl Into<Transport>,
        cfg: HostConfig,
        root_public: [u8; 32],
    ) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let secret = SecretKey::from_bytes(&load_or_create_seed(&dir.join("endpoint.key"))?);
        let state = State::load(&dir.join("state.json"))?;
        let mut cfg = cfg;
        cfg.audit_log.get_or_insert_with(|| dir.join("audit.log"));
        Self::build(secret, transport.into(), cfg, Root::Verifier(root_public), state).await
    }

    async fn build(
        secret: SecretKey,
        transport: Transport,
        cfg: HostConfig,
        root: Root,
        mut state: State,
    ) -> Result<Self> {
        if let Some(r) = &cfg.runner {
            if r.isolation() < cfg.min_isolation {
                bail!("runner {} gives {:?} isolation but this host requires at least {:?}", r.name(), r.isolation(), cfg.min_isolation);
            }
            r.preflight().await.with_context(|| format!("runner {} is not usable", r.name()))?;
        }
        if let Some(st) = &cfg.steer {
            if cfg.runner.is_none() {
                bail!("steering needs a runner");
            }
            st.check_against(&cfg.limits)?;
        }
        let attest_key = Arc::new(SigningKey::from_bytes(&secret.to_bytes()));
        let audit = Arc::new(match &cfg.audit_log {
            None => audit::Audit::off((*attest_key).clone()),
            Some(p) if p.as_os_str() == "-" => audit::Audit::stderr((*attest_key).clone()),
            Some(p) => audit::Audit::open(p, (*attest_key).clone())?,
        });
        audit.record(
            "host_start",
            None,
            json!({
                "host_key": attest_key.verifying_key().to_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "version": env!("CARGO_PKG_VERSION"),
                "transport": transport.label(),
                "runner": cfg.runner.as_ref().map(|r| r.name()).unwrap_or("none"),
                "isolation": cfg.runner.as_ref().map(|r| format!("{:?}", r.isolation())),
                "repo": cfg.repo.is_some(),
                "replica": matches!(root, Root::Verifier(_)),
                "scratch_quota_mib": cfg.scratch_quota.max_bytes >> 20,
                "max_push_mib": cfg.max_push_bytes >> 20,
            }),
        );
        let measurements = Arc::new(attest::measure_self(
            cfg.runner.as_ref().map(|r| r.name()).unwrap_or("none"),
            &format!(
                "isolation={:?};runtime={};limits={:?};grant_ttl={:?};repo={};workspace={}",
                cfg.runner.as_ref().map(|r| r.isolation()),
                cfg.runner.as_ref().map(|r| r.attestation()).unwrap_or_default(),
                cfg.limits,
                cfg.grant_ttl,
                cfg.repo.is_some(),
                cfg.workspace.is_some()
            ),
        ));
        let (endpoint, memory) = bind(secret, &transport).await?;
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
        let room = endpoint.id().to_string();
        let auth = Arc::new(match root {
            Root::Seed(seed) => Authority::from_seed(&room, &seed)?,
            Root::Fresh => Authority::new(&room),
            Root::Verifier(public) => Authority::verifier(&room, &public)?,
        });
        // Notices read from disk are only believed once they verify against this room's key.
        for text in std::mem::take(&mut state.pending) {
            match revoke::Notice::parse(&text, &auth) {
                Ok(n) => {
                    state.apply(&n, &text);
                }
                Err(e) => eprintln!("playbox: ignoring a stored revocation notice that does not verify: {e:#}"),
            }
        }
        let state = Arc::new(Mutex::new(state));
        let gossip = iroh_gossip::net::Gossip::builder().spawn(endpoint.clone());
        let repl = Arc::new(revoke::Replication::new(gossip.clone(), memory));
        let card = wire::agent_card(&room, &transport.label(), cfg.repo.is_some() || cfg.workspace.is_some(), cfg.runner.is_some(), &cfg.limits);
        let handler = RoomHandler {
            state: state.clone(),
            tasks: Arc::new(Semaphore::new(2)),
            limiter: limits::Limiter::new(cfg.abuse.clone()),
            conns: Arc::new(Semaphore::new(cfg.max_connections.max(1))),
            audit: audit.clone(),
            peer_conns: Arc::default(),
            pushed: Arc::default(),
            platform_calls: Arc::new(Semaphore::new(2)),
            auth: auth.clone(),
            done: Arc::default(),
            card: Arc::new(serde_json::to_value(card).context("agent card")?),
            attest_key,
            measurements,
            platform: cfg.platform.clone(),
            cfg,
        };
        let sync = revoke::SyncHandler { state: state.clone(), allowed: repl.allowed.clone() };
        let router = Router::builder(endpoint)
            .accept(ALPN, handler)
            .accept(iroh_gossip::net::GOSSIP_ALPN, gossip)
            .accept(SYNC_ALPN, sync)
            .spawn();
        Ok(Self { audit, router, state, auth, transport, repl })
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
        self.create_link_with(ttl, uses, LinkOpts::default())
    }

    /// Like [`Host::create_link`], optionally bound to one guest key and with or without IP addresses.
    pub fn create_link_with(&self, ttl: Duration, uses: u32, opts: LinkOpts) -> Result<Link> {
        let (invite, id) = self.auth.mint_invite(ttl.as_secs() as i64)?;
        {
            let mut st = self.state.lock().expect("state lock");
            st.invites.insert(id, Invite { uses_left: uses.max(1), bound_to: opts.bind_to });
            st.save();
        }
        let mut addr = self.addr();
        let has_relay = addr.addrs.iter().any(|a| matches!(a, iroh::TransportAddr::Relay(_)));
        if self.transport != Transport::Local && !opts.show_ips && has_relay {
            addr.addrs.retain(|a| !matches!(a, iroh::TransportAddr::Ip(_)));
        }
        Ok(Link { addr, invite, transport: Some(self.transport.clone()), bound: opts.bind_to.is_some() })
    }

    pub fn members(&self) -> Vec<EndpointId> {
        self.state.lock().expect("state lock").members.keys().copied().collect()
    }

    /// Revoke a member's grant (and every attenuation of it) and bar the key from rejoining.
    /// Takes effect here on the member's next request, and is signed with the room's root key and
    /// handed to revocation sync so other hosts of the room apply it too.
    /// Returns false for an unknown member, or on a replica (which cannot sign).
    pub fn revoke_member(&self, id: &EndpointId) -> bool {
        let mut st = self.state.lock().expect("state lock");
        let Some(grant_id) = st.members.get(id).cloned() else { return false };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let notice = revoke::Notice { grant_id, member: id.to_string(), issued_ms: now };
        let Ok(text) = notice.sign(&self.auth) else { return false };
        st.apply(&notice, &text);
        st.save();
        drop(st);
        self.audit.record(
            "revoke",
            Some(&id.to_string()),
            json!({ "grant": notice.grant_id.chars().take(8).collect::<String>(), "spread": true }),
        );
        let _ = self.repl.tx.send(text);
        true
    }

    /// Where this host's audit log is, if it keeps one in a file.
    pub fn audit_path(&self) -> Option<PathBuf> {
        self.audit.path().map(std::path::Path::to_path_buf)
    }

    /// The room's root public key. Give it to replicas (`Host::open_replica`).
    pub fn root_public(&self) -> [u8; 32] {
        self.auth.public_bytes()
    }

    /// Every signed revocation this host knows of.
    pub fn revocation_notices(&self) -> Vec<String> {
        self.state.lock().expect("state lock").notices.values().cloned().collect()
    }

    /// Start spreading revocations with `peers`, the other hosts of this room.
    ///
    /// New notices go out over iroh-gossip; every `interval` this host also asks each peer for all of
    /// theirs, so a host that was offline or missed a broadcast catches up. Peers are trusted only to
    /// *carry* notices (each is checked against the room's root key); only they may pull from this host,
    /// so list this host on theirs too. Call once.
    pub async fn enable_revocation_sync(&self, peers: Vec<EndpointAddr>, interval: Duration) -> Result<()> {
        self.repl
            .start(self.router.endpoint().clone(), self.state.clone(), self.auth.clone(), peers, interval)
            .await
    }

    pub async fn shutdown(self) -> Result<()> {
        self.repl.stop();
        self.router.shutdown().await.map_err(|e| anyhow!("{e}"))
    }
}

/// The full result of a task run on the host.
#[derive(Debug, Clone)]
pub struct TaskRun {
    /// The A2A task id; use it with [`Client::get_task`].
    pub task_id: String,
    pub runner: String,
    pub output: TaskOutput,
    /// Everything the command changed, as a git patch. Empty when the host keeps results in a
    /// versioned workspace instead.
    pub patch: Vec<u8>,
    /// Workspace hosts only: the host-side branch holding this member's work, the commit this task
    /// created (if it changed anything), and the paths it touched.
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub changed: Vec<String>,
}

pub struct Client {
    endpoint: Endpoint,
    conn: Connection,
    grant: Mutex<Option<String>>,
    next_id: AtomicI64,
    transport: Transport,
    /// Whether this connection has had a request accepted with an invite or grant, so the host will
    /// take a large request on it.
    warm: std::sync::atomic::AtomicBool,
}

impl Client {
    pub async fn connect(addr: EndpointAddr, secret: SecretKey, transport: impl Into<Transport>) -> Result<Self> {
        let transport = transport.into();
        let (endpoint, _) = bind(secret, &transport).await?;
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
        Ok(Self { endpoint, conn, grant: Mutex::new(None), next_id: AtomicI64::new(1), transport, warm: Default::default() })
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// The id of the host this client is connected to.
    pub fn host_id(&self) -> EndpointId {
        self.conn.remote_id()
    }

    /// Open a connection to another host of the same room (a replica), as the same member: the same
    /// identity key and the same grant, which any host that knows the room's public key accepts.
    /// The caller should vet the new host (`verify_host`) before sending it anything.
    pub async fn sibling(&self, addr: EndpointAddr) -> Result<Client> {
        let c = Client::connect(addr, self.endpoint.secret_key().clone(), self.transport.clone()).await?;
        if let Some(g) = self.grant() {
            c.set_grant(g);
        }
        Ok(c)
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

    /// One JSON-RPC call with this client's grant, returning the whole response (including any error code).
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<JsonRpcResponse> {
        let body = self.frame_bytes(method, params)?;
        // The host reads only a small first request from a connection that has not proved itself. If
        // this one is large and nothing has been accepted on the connection yet, say something small first.
        if body.len() > MAX_UNAUTHENTICATED_REQUEST && !self.warm.load(Ordering::Relaxed) && !wire::is_public(method) {
            let ping = Op::Echo { text: "warm-up".into() }.message(vec![]);
            let req = SendMessageRequest { message: ping, configuration: None, metadata: None, tenant: None };
            let small = self.frame_bytes(a2a::methods::SEND_MESSAGE, serde_json::to_value(req)?)?;
            let resp = self.exchange(&small, a2a::methods::SEND_MESSAGE).await.context("proving this connection before a large request")?;
            if let Some(e) = resp.error {
                bail!("could not prove this connection before a large request: {}", e.message);
            }
        }
        self.exchange(&body, method).await
    }

    fn frame_bytes(&self, method: &str, params: Value) -> Result<Vec<u8>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = wire::Frame::new(JsonRpcRequest::new(id.into(), method, Some(params)), self.grant().as_deref());
        Ok(serde_json::to_vec(&frame)?)
    }

    /// Send one already-built request and read the response.
    async fn exchange(&self, body: &[u8], method: &str) -> Result<JsonRpcResponse> {
        let (mut send, mut recv) = self.conn.open_bi().await?;
        send.write_all(body).await?;
        send.finish()?;
        let resp: JsonRpcResponse = serde_json::from_slice(&recv.read_to_end(MAX_FRAME).await?).context("bad response")?;
        if resp.error.is_none() && !wire::is_public(method) {
            self.warm.store(true, Ordering::Relaxed);
        }
        Ok(resp)
    }

    /// One JSON-RPC call with this client's grant; returns the `result`, or the error's message.
    /// The typed helpers below use it; call it directly for A2A methods that have no helper.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let resp = self.call_raw(method, params).await?;
        match (resp.result, resp.error) {
            (_, Some(e)) => Err(anyhow!("{}", e.message)),
            (Some(r), None) => Ok(r),
            (None, None) => Err(anyhow!("empty response")),
        }
    }

    /// Send a Playbox operation as an A2A message.
    async fn send(&self, op: &Op, extra: Vec<Part>) -> Result<SendMessageResponse> {
        let req = SendMessageRequest { message: op.message(extra), configuration: None, metadata: None, tenant: None };
        Ok(serde_json::from_value(self.call(a2a::methods::SEND_MESSAGE, serde_json::to_value(req)?).await?)?)
    }

    /// A task result that must have completed; otherwise the host's reason.
    async fn send_task(&self, what: &str, op: &Op, extra: Vec<Part>) -> Result<Task> {
        match self.send(op, extra).await.map_err(|e| anyhow!("{what} refused: {e:#}"))? {
            SendMessageResponse::Task(t) if t.status.state == TaskState::Completed => Ok(t),
            SendMessageResponse::Task(t) => bail!("{what} refused: {}", wire::status_text(&t)),
            SendMessageResponse::Message(m) => bail!("unexpected message instead of a task: {:?}", m.text()),
        }
    }

    /// Present the invite; returns this client's member id.
    pub async fn join(&self, invite: &str) -> Result<String> {
        let r = self.call(wire::METHOD_JOIN, json!({ "invite": invite })).await.map_err(|e| anyhow!("join refused: {e:#}"))?;
        let grant = r.get("grant").and_then(Value::as_str).ok_or_else(|| anyhow!("join response has no grant"))?;
        self.set_grant(grant.to_string());
        Ok(r.get("member").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    pub async fn echo(&self, text: &str) -> Result<String> {
        match self.send(&Op::Echo { text: text.to_string() }, vec![]).await.map_err(|e| anyhow!("echo refused: {e:#}"))? {
            SendMessageResponse::Message(m) => Ok(m.text().unwrap_or_default().to_string()),
            SendMessageResponse::Task(t) => bail!("echo refused: {}", wire::status_text(&t)),
        }
    }

    /// The host's public agent card.
    pub async fn agent_card(&self) -> Result<a2a::AgentCard> {
        Ok(serde_json::from_value(self.call(wire::METHOD_AGENT_CARD, Value::Null).await?)?)
    }

    /// Ask the host to attest to what it runs, and check the answer.
    ///
    /// The statement must be signed by the key this connection already authenticated, must echo a
    /// fresh nonce, and the tier it *achieves* (see [`playbox_core::attest`]) must reach `min`.
    /// `verifiers` check platform evidence; with none, the achieved tier is T0.
    pub async fn verify_host(&self, min: Tier, verifiers: &[&dyn PlatformVerifier]) -> Result<Verified> {
        let nonce: String = random_bytes()?[..16].iter().map(|b| format!("{b:02x}")).collect();
        let r = self.call(wire::METHOD_ATTESTATION, json!({ "nonce": nonce })).await?;
        let text = r.get("statement").and_then(Value::as_str).ok_or_else(|| anyhow!("no statement in response"))?;
        let mut v = attest::verify(text, verifiers).map_err(|e| anyhow!("host attestation invalid: {e}"))?;
        if v.statement.subject != *self.conn.remote_id().as_bytes() {
            bail!("host attestation is for a different key than the one this connection authenticated");
        }
        if v.statement.nonce != nonce {
            bail!("host attestation does not answer our nonce (replayed?)");
        }
        // Only now, with the host's identity and freshness settled, put its hardware to the test.
        if let Some(ch) = v.challenge.clone() {
            let req: String = ch.payload.iter().map(|b| format!("{b:02x}")).collect();
            match self.call(wire::METHOD_ACTIVATE, json!({ "request": req })).await {
                Ok(r) => {
                    let h = r.get("response").and_then(Value::as_str).unwrap_or_default();
                    let bytes: Vec<u8> = (0..h.len() / 2).map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect();
                    v.complete_challenge(&bytes);
                }
                Err(e) => v.notes.push(format!("host could not answer the hardware challenge: {e:#}")),
            }
        }
        if v.achieved < min {
            bail!("host achieves {} but {} was required ({})", v.achieved, min, v.notes.join("; "));
        }
        Ok(v)
    }

    /// A task this member created earlier (byte payloads are not kept; see [`wire::without_bytes`]).
    pub async fn get_task(&self, id: &str) -> Result<Task> {
        Ok(serde_json::from_value(self.call(a2a::methods::GET_TASK, serde_json::to_value(GetTaskRequest { id: id.into(), history_length: None, tenant: None })?).await?)?)
    }

    pub async fn list_tasks(&self) -> Result<Vec<Task>> {
        let r: ListTasksResponse = serde_json::from_value(self.call(a2a::methods::LIST_TASKS, json!({})).await?)?;
        Ok(r.tasks)
    }

    /// Fetch `refname` from the room; returns its commit id and a git bundle.
    pub async fn pull(&self, refname: &str) -> Result<(String, Vec<u8>)> {
        let t = self.send_task("pull", &Op::Pull { refname: refname.to_string() }, vec![]).await?;
        let head = wire::meta(&t, "head").and_then(Value::as_str).unwrap_or_default().to_string();
        let bundle = wire::artifact_bytes(&t, "bundle").ok_or_else(|| anyhow!("pull returned no bundle"))?;
        Ok((head, bundle))
    }

    /// Send a bundle holding `branch`. Returns the ref it was stored under on the host.
    pub async fn push(&self, branch: &str, bundle: &[u8]) -> Result<String> {
        let part = Part::raw(bundle.to_vec()).with_filename("push.bundle").with_media_type(wire::MEDIA_BUNDLE);
        let t = self.send_task("push", &Op::Push { branch: branch.to_string() }, vec![part]).await?;
        Ok(wire::meta(&t, "reference").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    /// Run a task on a checkout of `base`; returns the runner name, its output and the patch.
    /// A command that exits non-zero or times out still returns `Ok`; check `exit_code` / `timed_out`.
    pub async fn run_task(&self, base: &str, spec: TaskSpec) -> Result<(String, TaskOutput, Vec<u8>)> {
        let r = self.run_task_full(base, spec).await?;
        Ok((r.runner, r.output, r.patch))
    }

    /// Like [`Client::run_task`], with the task id and workspace commit details.
    pub async fn run_task_full(&self, base: &str, spec: TaskSpec) -> Result<TaskRun> {
        let op = Op::Run { base: base.to_string(), spec };
        let t = match self.send(&op, vec![]).await.map_err(|e| anyhow!("task refused: {e:#}"))? {
            SendMessageResponse::Task(t) => t,
            SendMessageResponse::Message(m) => bail!("unexpected message instead of a task: {:?}", m.text()),
        };
        let exit_code = wire::meta(&t, "exitCode").and_then(Value::as_i64).map(|c| c as i32);
        let timed_out = wire::meta(&t, "timedOut").and_then(Value::as_bool).unwrap_or(false);
        let ran = t.status.state == TaskState::Completed || exit_code.is_some() || timed_out;
        if !ran {
            bail!("task refused: {}", wire::status_text(&t));
        }
        let text = |k: &str| wire::meta(&t, k).and_then(Value::as_str).map(str::to_string);
        Ok(TaskRun {
            task_id: t.id.clone(),
            runner: text("runner").unwrap_or_default(),
            output: TaskOutput {
                exit_code,
                timed_out,
                stdout: wire::artifact_text(&t, "stdout").unwrap_or_default(),
                stderr: wire::artifact_text(&t, "stderr").unwrap_or_default(),
                policy_digest: text("policyDigest"),
            },
            patch: wire::artifact_bytes(&t, "patch").unwrap_or_default(),
            branch: text("branch"),
            commit: text("commit"),
            changed: wire::meta(&t, "changed")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        })
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
