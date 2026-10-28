//! Playbox networking: a host that admits members by invite and a client that joins by link.
//!
//! Peers speak A2A v1 JSON-RPC over iroh streams; see [`wire`] for the binding.

pub mod grants;
pub mod transport;
pub mod wire;
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
use playbox_runner::{Limits, Runner, TaskOutput, TaskSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wire::Op;
use std::{
    collections::{HashMap, HashSet, VecDeque},
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
    /// Versioned workspaces. When set, `RunTask` runs against the workspace branch named by `base`:
    /// each member gets a branch `members/<id>` forked from it, and every task becomes one commit
    /// there (readable through this handle). Tasks serialise on this lock. Takes precedence over `repo`.
    pub workspace: Option<Arc<tokio::sync::Mutex<playbox_core::workspace::Repo>>>,
}

#[derive(Clone)]
struct RoomHandler {
    state: Arc<Mutex<State>>,
    cfg: HostConfig,
    tasks: Arc<Semaphore>,
    auth: Arc<Authority>,
    /// Recent tasks per member, with byte payloads dropped. Not persisted.
    done: Arc<Mutex<HashMap<EndpointId, VecDeque<Task>>>>,
    card: Arc<Value>,
}

impl std::fmt::Debug for RoomHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomHandler")
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
            wire::METHOD_JOIN => {
                let invite = params.get("invite").and_then(Value::as_str).ok_or_else(|| {
                    A2AError::invalid_params("missing `invite`").to_jsonrpc_error()
                })?;
                self.join(from, invite.to_string()).map_err(unauthorized)
            }
            methods::SEND_MESSAGE => {
                let req: SendMessageRequest = serde_json::from_value(params).map_err(invalid)?;
                let op = Op::from_message(&req.message).map_err(|e| A2AError::invalid_params(format!("{e:#}")).to_jsonrpc_error())?;
                // A plain A2A message with no Playbox operation only needs membership, like echo.
                let (name, needs) = match &op {
                    Some(op) => (op.name(), op.needs()),
                    None => ("echo", grants::Needs::default()),
                };
                self.authorize(frame.grant(), from, name, &needs)?;
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
            Some(Op::Pull { refname }) => finish(self.pull(&refname).await),
            Some(Op::Push { branch }) => finish(self.push(from, &branch, wire::raw_part(msg)).await),
            Some(Op::Run { base, spec }) => finish(self.run(from, &base, spec).await),
        };
        self.remember(from, &task);
        SendMessageResponse::Task(task)
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
        let short: String = from.to_string().chars().take(16).collect();
        let reference = format!("refs/playbox/members/{short}/{branch}");
        let head = playbox_git::bundle_import(&repo, bundle, &format!("refs/heads/{branch}"), &reference).await?;
        Ok(wire::task(TaskState::Completed, None, vec![], vec![("reference", json!(reference)), ("head", json!(head))]))
    }

    async fn run(&self, from: EndpointId, base: &str, spec: TaskSpec) -> Result<Task> {
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
            let run = playbox_runner::workspace::run_in_workspace(&mut repo, &mut wsp, runner.as_ref(), &spec, &from.to_string(), &format!("rpc-{now}"), now).await?;
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
            let dir = playbox_git::worktree_add(&repo, &commit).await?;
            let result = async {
                let output = runner.run(&spec, &dir).await?;
                let patch = playbox_git::diff_patch(&dir, &commit).await?;
                anyhow::Ok((output, patch))
            }
            .await;
            let _ = playbox_git::worktree_remove(&repo, &dir).await;
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
        if let Some(c) = output.exit_code {
            meta.push(("exitCode", json!(c)));
        }
        meta.extend(extra);
        Ok(wire::task(if ok { TaskState::Completed } else { TaskState::Failed }, text, artifacts, meta))
    }

    async fn dispatch(&self, from: EndpointId, bytes: &[u8]) -> JsonRpcResponse {
        let frame: wire::Frame = match serde_json::from_slice(bytes) {
            Ok(f) => f,
            Err(e) => return JsonRpcResponse::error(JsonRpcId::Null, A2AError::parse_error(e.to_string()).to_jsonrpc_error()),
        };
        let id = frame.rpc.id.clone();
        match self.route(from, &frame).await {
            Ok(v) => JsonRpcResponse::success(id, v),
            Err(e) => JsonRpcResponse::error(id, e),
        }
    }
}

impl ProtocolHandler for RoomHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let from = conn.remote_id();
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let Ok(bytes) = recv.read_to_end(MAX_FRAME).await else { break };
            let resp = self.dispatch(from, &bytes).await;
            let Ok(out) = serde_json::to_vec(&resp) else { break };
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
        let card = wire::agent_card(&room, &transport.label(), cfg.repo.is_some() || cfg.workspace.is_some(), cfg.runner.is_some(), &cfg.limits);
        let handler = RoomHandler {
            state: state.clone(),
            tasks: Arc::new(Semaphore::new(2)),
            auth: auth.clone(),
            done: Arc::default(),
            card: Arc::new(serde_json::to_value(card).context("agent card")?),
            cfg,
        };
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
        Ok(Self { endpoint, conn, grant: Mutex::new(None), next_id: AtomicI64::new(1) })
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

    /// One JSON-RPC call with this client's grant, returning the whole response (including any error code).
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<JsonRpcResponse> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = wire::Frame::new(JsonRpcRequest::new(id.into(), method, Some(params)), self.grant().as_deref());
        let (mut send, mut recv) = self.conn.open_bi().await?;
        send.write_all(&serde_json::to_vec(&frame)?).await?;
        send.finish()?;
        serde_json::from_slice(&recv.read_to_end(MAX_FRAME).await?).context("bad response")
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
