//! The room host: its configuration, how it is started, and what its owner can do with it.

use crate::bind::{bind, Bind};
use crate::grants::Authority;
use crate::handler::{drop_all_sessions, drop_member_session, RoomHandler, SessionMap};
use crate::keys::load_or_create_seed;
use crate::link::{Link, LinkOpts};
use crate::state::{Admit, Invite, State};
use crate::sync::LockExt;
use crate::{
    audit, browse, capabilities, chat, conversations, latent, limits, revoke, shared_memory, steer,
    wire, Transport, ALPN, AUTHENTICATED_PATIENCE, SYNC_ALPN,
};
use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::SigningKey;
use iroh::{protocol::Router, EndpointAddr, EndpointId, SecretKey};
use playbox_core::attest::{self, PlatformProvider};
use playbox_runner::{Isolation, Limits, Runner};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

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
    /// How long a connection that has proved itself may sit silent before the host closes it. Default 15 minutes.
    /// A member whose connection was closed this way reconnects with [`Client::reconnect`].
    pub authenticated_patience: Duration,
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
    /// Hub latent messages: hub allowlist, post rate, expiry and where the mailbox is kept.
    pub latent: latent::LatentPolicy,
    /// Where the room's feed is kept across restarts. `None`: `<state dir>/chat.jsonl` for a host opened with a state
    /// directory, and memory only for an ephemeral host.
    pub chat_log: Option<PathBuf>,
    /// The room's shared memory (a Hindsight sidecar, kept by the host). `None`: `remember` and `recall` are refused.
    pub memory: Option<Arc<shared_memory::SharedMemory>>,
    /// A browser members may read allowed pages through. `None`: `browse` is refused.
    pub browse: Option<Arc<browse::Browser>>,
    /// Operations offered to members by name (see [`capabilities`]). Needs `runner`; each member's grant lists the names.
    pub capabilities: Vec<capabilities::Capability>,
    /// Where calls are kept across restarts. `None`: `<state dir>/calls.json` for a host opened with a state directory.
    pub calls_store: Option<PathBuf>,
    /// Where conversations are kept across restarts. `None`: `<state dir>/conversations.json` for a host opened with a state directory.
    pub conversations_store: Option<PathBuf>,
    /// Where this host keeps its copy of the room log (`roomlog.jsonl` in the state directory).
    pub roomlog_store: Option<PathBuf>,
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
            authenticated_patience: AUTHENTICATED_PATIENCE,
            repo: None,
            runner: None,
            limits: Limits::default(),
            grant_ttl: None,
            workspace: None,
            platform: None,
            steer: None,
            abuse: limits::AbuseLimits::default(),
            latent: latent::LatentPolicy::default(),
            chat_log: None,
            memory: None,
            browse: None,
            capabilities: Vec::new(),
            calls_store: None,
            conversations_store: None,
            roomlog_store: None,
        }
    }
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

/// Whether `id` may still run a capability: in the room, not banned, grant not revoked and not past its expiry.
fn member_standing(st: &State, id: &EndpointId, now: i64) -> Result<(), String> {
    if st.banned.contains(id) {
        return Err("the member was revoked".into());
    }
    match st.members.get(id) {
        None => Err("the member is no longer in the room".into()),
        Some(grant) if st.revoked.contains(grant) => Err("the member's grant was revoked".into()),
        Some(_) if st.admits.get(id).is_some_and(|a| now >= a.expires) => {
            Err("the member's grant has expired".into())
        }
        Some(_) => Ok(()),
    }
}

type RevokeHook = Arc<dyn Fn(&EndpointId) + Send + Sync>;

pub struct Host {
    audit: Arc<audit::Audit>,
    router: Router,
    state: Arc<Mutex<State>>,
    auth: Arc<Authority>,
    transport: Transport,
    repl: Arc<revoke::Replication>,
    memory: Option<Arc<shared_memory::SharedMemory>>,
    caps: Arc<capabilities::Engine>,
    roomlog: Arc<crate::roomsync::RoomLogNode>,
    runner: Option<Arc<dyn Runner>>,
    sessions: SessionMap,
    handler: RoomHandler,
    revoke_hooks: Arc<Mutex<Vec<RevokeHook>>>,
    /// Holds the exclusive lock on `<state dir>/host.lock` for as long as the host lives.
    _dir_lock: Option<std::fs::File>,
}

/// Take the state directory's exclusive lock, or say that another host has it. The OS drops the lock
/// when the process exits, so a crashed host never blocks its restart.
fn lock_dir(dir: &std::path::Path) -> Result<std::fs::File> {
    let path = dir.join("host.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "state directory {} is in use by another host",
            dir.display()
        ),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking {}", path.display()))
        }
    }
}

impl Host {
    /// `transport` may be a [`Transport`] or a bool (`true` = local, `false` = public iroh).
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn start(secret: SecretKey, transport: impl Into<Transport>) -> Result<Self> {
        Self::start_with(secret, transport, HostConfig::default()).await
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn start_with(
        secret: SecretKey,
        transport: impl Into<Transport>,
        cfg: HostConfig,
    ) -> Result<Self> {
        Self::build(secret, transport.into(), cfg, Root::Fresh, State::default()).await
    }

    /// Start a host that keeps its identity, signing key, invites, members and bans in `dir`.
    /// Restarting with the same `dir` gives the same endpoint id, keeps issued grants valid, and keeps
    /// revoked members out. Links created before the restart still work if their address does.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn open(
        dir: &std::path::Path,
        transport: impl Into<Transport>,
        cfg: HostConfig,
    ) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let lock = lock_dir(dir)?;
        let secret = SecretKey::from_bytes(&load_or_create_seed(&dir.join("endpoint.key"))?);
        let root_seed = load_or_create_seed(&dir.join("root.key"))?;
        let state = State::load(&dir.join("state.json"))?;
        let mut cfg = cfg;
        cfg.audit_log.get_or_insert_with(|| dir.join("audit.log"));
        cfg.latent
            .store
            .get_or_insert_with(|| dir.join("latent.json"));
        cfg.calls_store
            .get_or_insert_with(|| dir.join("calls.json"));
        cfg.conversations_store
            .get_or_insert_with(|| dir.join("conversations.json"));
        cfg.chat_log.get_or_insert_with(|| dir.join("chat.jsonl"));
        cfg.roomlog_store
            .get_or_insert_with(|| dir.join("roomlog.jsonl"));
        let mut host =
            Self::build(secret, transport.into(), cfg, Root::Seed(root_seed), state).await?;
        host._dir_lock = Some(lock);
        Ok(host)
    }

    /// Start a **replica**: another host of an existing room. It knows the room's root *public* key,
    /// so it accepts the room's grants and checks its revocations, but it holds no signing key: it
    /// cannot admit members, issue grants, or revoke. Give it the same `--sync-peer`s as the primary
    /// (see [`Host::enable_revocation_sync`]) so revocations reach it.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn open_replica(
        dir: &std::path::Path,
        transport: impl Into<Transport>,
        cfg: HostConfig,
        root_public: [u8; 32],
    ) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let lock = lock_dir(dir)?;
        let secret = SecretKey::from_bytes(&load_or_create_seed(&dir.join("endpoint.key"))?);
        let state = State::load(&dir.join("state.json"))?;
        let mut cfg = cfg;
        cfg.audit_log.get_or_insert_with(|| dir.join("audit.log"));
        cfg.latent
            .store
            .get_or_insert_with(|| dir.join("latent.json"));
        cfg.calls_store
            .get_or_insert_with(|| dir.join("calls.json"));
        cfg.conversations_store
            .get_or_insert_with(|| dir.join("conversations.json"));
        cfg.chat_log.get_or_insert_with(|| dir.join("chat.jsonl"));
        cfg.roomlog_store
            .get_or_insert_with(|| dir.join("roomlog.jsonl"));
        let mut host = Self::build(
            secret,
            transport.into(),
            cfg,
            Root::Verifier(root_public),
            state,
        )
        .await?;
        host._dir_lock = Some(lock);
        Ok(host)
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
                bail!(
                    "runner {} gives {:?} isolation but this host requires at least {:?}",
                    r.name(),
                    r.isolation(),
                    cfg.min_isolation
                );
            }
            r.preflight()
                .await
                .with_context(|| format!("runner {} is not usable", r.name()))?;
        }
        if let Some(st) = &cfg.steer {
            if cfg.runner.is_none() {
                bail!("steering needs a runner");
            }
            st.check_against(&cfg.limits)?;
        }
        let attest_key = Arc::new(SigningKey::from_bytes(&secret.to_bytes()));
        let audit = Arc::new(
            match &cfg.audit_log {
                None => audit::Audit::off((*attest_key).clone()),
                Some(p) if p.as_os_str() == "-" => audit::Audit::stderr((*attest_key).clone()),
                Some(p) => audit::Audit::open(p, (*attest_key).clone())?,
            }
            .with_chat(
                cfg.chat_log
                    .as_deref()
                    .map_or_else(chat::Chat::new, chat::Chat::open),
            ),
        );
        audit.record(
            "host_start",
            None,
            json!({
                "host_key": attest_key.verifying_key().to_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "version": env!("CARGO_PKG_VERSION"),
                "transport": transport.label(),
                "runner": cfg.runner.as_ref().map_or("none", |r| r.name()),
                "isolation": cfg.runner.as_ref().map(|r| format!("{:?}", r.isolation())),
                "repo": cfg.repo.is_some(),
                "replica": matches!(root, Root::Verifier(_)),
                "scratch_quota_mib": cfg.scratch_quota.max_bytes >> 20,
                "max_push_mib": cfg.max_push_bytes >> 20,
            }),
        );
        let measurements = Arc::new(attest::measure_self(
            cfg.runner.as_ref().map_or("none", |r| r.name()),
            &format!(
                "isolation={:?};runtime={};limits={:?};grant_ttl={:?};repo={};workspace={}",
                cfg.runner.as_ref().map(|r| r.isolation()),
                cfg.runner
                    .as_ref()
                    .map(|r| r.attestation())
                    .unwrap_or_default(),
                cfg.limits,
                cfg.grant_ttl,
                cfg.repo.is_some(),
                cfg.workspace.is_some()
            ),
        ));
        let (endpoint, memory) = bind(secret, &transport, Bind::Host).await?;
        if !transport.needs_online_wait() {
            // No relay to wait for: wait until the endpoint knows at least one direct address,
            // or links created right after start could carry none.
            for _ in 0..100 {
                if !endpoint.addr().addrs.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } else if tokio::time::timeout(Duration::from_secs(20), endpoint.online())
            .await
            .is_err()
        {
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
                Err(e) => eprintln!(
                    "playbox: ignoring a stored revocation notice that does not verify: {e:#}"
                ),
            }
        }
        let state = Arc::new(Mutex::new(state));
        let gossip = iroh_gossip::net::Gossip::builder().spawn(endpoint.clone());
        let repl = Arc::new(revoke::Replication::new(gossip.clone(), memory));
        let card = wire::agent_card(
            &room,
            &transport.label(),
            cfg.repo.is_some() || cfg.workspace.is_some(),
            cfg.runner.is_some(),
            &cfg.limits,
        );
        let memory = cfg.memory.clone();
        let standing_state = state.clone();
        let caps = Arc::new(
            capabilities::Engine::new(
                capabilities::Book::new(cfg.capabilities.clone(), cfg.calls_store.clone())?,
                cfg.runner.clone(),
                audit.clone(),
                cfg.scratch_quota,
                cfg.limits.clone(),
            )
            .with_standing(Arc::new(move |member: &str, _capability: &str| {
                let id: EndpointId = member
                    .parse()
                    .map_err(|_| "the member id is not valid".to_string())?;
                let st = standing_state.lock_recover();
                member_standing(&st, &id, crate::grants::now_secs())
            })),
        );
        let roomlog = crate::roomsync::RoomLogNode::new(
            auth.public_bytes(),
            (*attest_key).clone(),
            repl.allowed.clone(),
            cfg.roomlog_store.clone(),
        )?;
        let runner_ref = cfg.runner.clone();
        let sessions: SessionMap = Arc::default();
        let live_limits = Arc::new(Mutex::new(cfg.limits.clone()));
        let handler = RoomHandler {
            roomlog: roomlog.clone(),
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
            replays: Arc::default(),
            handoffs: Arc::default(),
            live_limits,
            live_policy_updated: Arc::default(),
            card: Arc::new(serde_json::to_value(card).context("agent card")?),
            attest_key,
            measurements,
            platform: cfg.platform.clone(),
            latent: Arc::new(latent::Mailbox::new(cfg.latent.clone())),
            caps: caps.clone(),
            convs: Arc::new(conversations::Book::new(cfg.conversations_store.clone())?),
            sessions: sessions.clone(),
            cfg,
        };
        let sync = revoke::SyncHandler::new(state.clone(), repl.allowed.clone());
        let router = Router::builder(endpoint)
            .accept(ALPN, handler.clone())
            .accept(iroh_gossip::net::GOSSIP_ALPN, gossip)
            .accept(SYNC_ALPN, sync)
            .accept(
                crate::roomsync::ROOMLOG_ALPN,
                crate::roomsync::RoomLogHandler::new(roomlog.clone()),
            )
            .spawn();
        Ok(Self {
            audit,
            router,
            state,
            auth,
            transport,
            repl,
            memory,
            caps,
            roomlog,
            runner: runner_ref,
            sessions,
            handler,
            revoke_hooks: Arc::new(Mutex::new(Vec::new())),
            _dir_lock: None,
        })
    }

    /// Live policy guardrails and real-time budget burn snapshot for this room.
    #[must_use]
    pub fn policy_snapshot(&self) -> serde_json::Value {
        self.handler.policy_snapshot()
    }

    /// Update the room's live policy guardrails and budgets without restarting the host.
    ///
    /// # Errors
    ///
    /// Returns an error if any supplied limit is out of range.
    pub fn update_policy(&self, patch: &serde_json::Value) -> Result<serde_json::Value> {
        let mut lim = self.handler.live_limits.lock_recover().clone();
        let mut abuse = self.handler.limiter.limits();
        if let Some(cpus) = patch.get("cpus").and_then(serde_json::Value::as_f64) {
            if !cpus.is_finite() || !(0.1..=256.0).contains(&cpus) {
                bail!("cpus must be between 0.1 and 256.0");
            }
            lim.cpus = cpus as f32;
        }
        if let Some(mem) = patch.get("mem_mib").and_then(serde_json::Value::as_u64) {
            if !(16..=1_048_576).contains(&mem) {
                bail!("mem_mib must be at least 16 MiB");
            }
            lim.mem_mib = mem as u32;
        }
        if let Some(tmo) = patch
            .get("timeout_secs")
            .and_then(serde_json::Value::as_u64)
        {
            if !(1..=2_592_000).contains(&tmo) {
                bail!("timeout_secs must be between 1 and 2592000");
            }
            lim.timeout_secs = tmo;
        }
        if let Some(net) = patch.get("network").and_then(serde_json::Value::as_bool) {
            lim.network = net;
        }
        let str_list = |k: &str| -> Option<Vec<String>> {
            patch
                .get(k)
                .and_then(serde_json::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty() && !s.contains('/'))
                        .map(str::to_string)
                        .collect()
                })
        };
        if let Some(hosts) = str_list("allowed_hosts") {
            lim.allowed_hosts = hosts;
        }
        if let Some(secs) = str_list("allowed_secrets") {
            lim.allowed_secrets = secs;
        }
        if let Some(eg) = str_list("allowed_egress") {
            lim.allowed_egress = eg;
        }
        if let Some(tb) = patch.get("task_budget").and_then(serde_json::Value::as_u64) {
            if tb == 0 {
                bail!("task_budget must be at least 1");
            }
            abuse.task_budget = tb;
        }
        if let Some(mm) = patch.get("max_members").and_then(serde_json::Value::as_u64) {
            if mm == 0 {
                bail!("max_members must be at least 1");
            }
            abuse.max_members = mm as usize;
        }
        if let Some(mc) = patch
            .get("max_concurrent")
            .and_then(serde_json::Value::as_u64)
        {
            if mc == 0 {
                bail!("max_concurrent must be at least 1");
            }
            abuse.max_concurrent_per_member = mc as usize;
        }
        if let Some(mt) = patch
            .get("max_tasks_per_minute")
            .and_then(serde_json::Value::as_u64)
        {
            if mt == 0 {
                bail!("max_tasks_per_minute must be at least 1");
            }
            abuse.max_tasks_per_window = mt as usize;
        }
        if let Some(rev) = patch
            .get("memory_review")
            .and_then(serde_json::Value::as_bool)
        {
            if let Some(mem) = &self.memory {
                mem.set_review(rev);
            }
        }
        if let Some(caps_obj) = patch
            .get("capability_approvals")
            .and_then(serde_json::Value::as_object)
        {
            for (name, val) in caps_obj {
                if let Some(apv) = val.as_bool() {
                    self.caps.book.set_approval(name, apv);
                }
            }
        }
        let cap_names = self.caps.book.names();
        let allowed_summary: Vec<String> = lim
            .allowed_hosts
            .iter()
            .cloned()
            .chain(lim.allowed_secrets.iter().map(|n| format!("secret/{n}")))
            .chain(lim.allowed_egress.iter().map(|n| format!("egress/{n}")))
            .chain(cap_names.iter().map(|n| format!("cap/{n}")))
            .collect();
        *self.handler.live_limits.lock_recover() = lim.clone();
        self.handler.limiter.set_limits(abuse.clone());
        self.handler
            .live_policy_updated
            .store(true, std::sync::atomic::Ordering::Relaxed);
        {
            let mut st = self.state.lock_recover();
            for admit in st.admits.values_mut() {
                admit.cpu_milli = (f64::from(lim.cpus) * 1000.0).ceil() as i64;
                admit.mem_mib = i64::from(lim.mem_mib);
                admit.timeout_secs = i64::try_from(lim.timeout_secs).unwrap_or(i64::MAX);
                admit.net = lim.network;
                admit.allowed = allowed_summary.clone();
            }
            st.save();
        }
        let snap = self.handler.policy_snapshot();
        self.audit.record(
            "policy_update",
            None,
            json!({
                "ok": true,
                "policy_digest": snap["policy_digest"],
                "cpus": lim.cpus,
                "mem_mib": lim.mem_mib,
                "timeout_secs": lim.timeout_secs,
                "network": lim.network,
                "task_budget": abuse.task_budget,
                "max_concurrent": abuse.max_concurrent_per_member,
                "max_tasks_per_minute": abuse.max_tasks_per_window,
                "memory_review": self.memory.as_ref().is_some_and(|m| m.review()),
            }),
        );
        Ok(snap)
    }

    /// Inspect or replay a task recorded on this host.
    pub async fn replay(&self, task_id: Option<&str>, run: bool) -> Result<serde_json::Value> {
        self.handler
            .replay_op(None, task_id, run)
            .await
            .map_err(|e| anyhow!(e))
    }

    /// Sign (if unsigned) and store a structured cross-agent handoff checkpoint as the host.
    pub fn checkpoint_put(
        &self,
        mut cp: crate::card::HandoffCheckpoint,
    ) -> Result<serde_json::Value> {
        let ep = self.router.endpoint();
        let sk = ep.secret_key().clone();
        cp.room = ep.id().to_string();
        if cp.issued_ms == 0 {
            cp.issued_ms = crate::chat::now_ms();
        }
        if cp.policy_digest.is_empty() {
            cp.policy_digest = self.handler.policy_digest();
        }
        if cp.budget_remaining.is_null()
            || cp
                .budget_remaining
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        {
            cp.budget_remaining = json!(self.handler.limiter.usage());
        }
        if cp.sig.is_empty() {
            cp.sign_with(ep.id().as_bytes(), |m| sk.sign(m).to_bytes())
                .map_err(|e| anyhow!(e))?;
        }
        self.handler
            .checkpoint_put(&ep.id().to_string(), &mut cp)
            .map_err(|e| anyhow!(e))
    }

    /// List structured cross-agent handoff checkpoints stored on this host.
    #[must_use]
    pub fn checkpoint_list(&self, id: Option<&str>) -> serde_json::Value {
        self.handler.checkpoint_list(id)
    }

    /// Shared room memory ledger snapshot and provenance/taint state.
    #[must_use]
    pub fn memory_snapshot(&self) -> serde_json::Value {
        let Some(mem) = &self.memory else {
            return json!({
                "enabled": false,
                "review": false,
                "entries": [],
            });
        };
        let mut entries = mem.entries();
        entries.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then(a.doc.cmp(&b.doc)));
        json!({
            "enabled": true,
            "review": mem.review(),
            "entries": entries,
        })
    }

    /// Owner or host actions on the room's shared memory (remember owner-endorsed fact, endorse, accept, quarantine, recall).
    pub async fn memory_action(&self, req: &serde_json::Value) -> Result<serde_json::Value> {
        let Some(mem) = &self.memory else {
            bail!("this room has no shared memory configured");
        };
        let action = req
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("list");
        match action {
            "list" => Ok(self.memory_snapshot()),
            "remember" => {
                let text = req
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("missing `text`"))?;
                let key = req.get("key").and_then(serde_json::Value::as_str);
                let task_id = req.get("task_id").and_then(serde_json::Value::as_str);
                let commit = req.get("commit").and_then(serde_json::Value::as_str);
                let endorsed = req
                    .get("endorsed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                let r = mem
                    .remember_with_provenance("host", text, key, task_id, commit, endorsed)
                    .await
                    .map_err(|e| anyhow!(e))?;
                Ok(json!({
                    "doc": r.doc,
                    "status": r.status,
                    "endorsed": endorsed,
                    "taint": shared_memory::taint_label("host", endorsed, task_id, commit),
                }))
            }
            "endorse" | "unendorse" => {
                let doc = req
                    .get("doc")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("missing `doc`"))?;
                let endorsed = action == "endorse"
                    && req
                        .get("endorsed")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                let e = mem.set_endorsed(doc, endorsed).await?;
                Ok(json!({ "ok": true, "entry": e }))
            }
            "accept" | "quarantine" => {
                let doc = req
                    .get("doc")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("missing `doc`"))?;
                let status = if action == "accept" {
                    shared_memory::Status::Accepted
                } else {
                    shared_memory::Status::Quarantined
                };
                mem.set_status(doc, status).await?;
                Ok(json!({ "ok": true, "doc": doc, "status": status.as_str() }))
            }
            "recall" => {
                let query = req
                    .get("query")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("*");
                let max_tokens = req
                    .get("max_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| n.min(u64::from(u32::MAX)) as u32);
                let author = req.get("author").and_then(serde_json::Value::as_str);
                let trusted_only = req
                    .get("trusted_only")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let mut hits = mem
                    .recall(query, max_tokens, author, &|_| true)
                    .await
                    .map_err(|e| anyhow!(e))?;
                if trusted_only {
                    hits.retain(|h| {
                        h.endorsed
                            || h.author == "host"
                            || h.taint == "owner_endorsed"
                            || h.taint == "verified"
                    });
                }
                Ok(json!({ "hits": hits }))
            }
            other => bail!("unknown memory action {other:?}"),
        }
    }

    /// Calls waiting for the owner's approval.
    #[must_use]
    pub fn pending_calls(&self) -> Vec<capabilities::Call> {
        self.caps.book.pending()
    }

    /// Approve or deny a waiting call (`member/id`, or the id alone if only one waiting call has it).
    ///
    /// # Errors
    ///
    /// Fails if no single waiting call matches.
    pub fn decide_call(&self, which: &str, approve: bool) -> Result<capabilities::Call> {
        self.caps.decide(which, approve)
    }

    #[must_use]
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
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn create_link(&self, ttl: Duration, uses: u32) -> Result<Link> {
        self.create_link_with(ttl, uses, LinkOpts::default())
    }

    /// Like [`Host::create_link`], optionally bound to one guest key and with or without IP addresses.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned, which only happens after another thread panicked while holding it.
    pub fn create_link_with(&self, ttl: Duration, uses: u32, opts: LinkOpts) -> Result<Link> {
        let (invite, id) = self
            .auth
            .mint_invite(ttl.as_secs().try_into().unwrap_or(i64::MAX))?;
        {
            let mut st = self.state.lock_recover();
            st.invites.insert(
                id,
                Invite {
                    uses_left: uses.max(1),
                    bound_to: opts.bind_to,
                },
            );
            st.save();
        }
        let mut addr = self.addr();
        let has_relay = addr
            .addrs
            .iter()
            .any(|a| matches!(a, iroh::TransportAddr::Relay(_)));
        if self.transport != Transport::Local && !opts.show_ips && has_relay {
            addr.addrs
                .retain(|a| !matches!(a, iroh::TransportAddr::Ip(_)));
        }
        Ok(Link {
            addr,
            invite,
            transport: Some(self.transport.clone()),
            bound: opts.bind_to.is_some(),
            hosts: self
                .with_log_hosts(&[])
                .into_iter()
                .take(crate::link::MAX_HINTS)
                .collect(),
        })
    }

    /// This host's endpoint id: the room id agent cards are bound to, and the key behind the host's own messages.
    #[must_use]
    pub fn endpoint_id(&self) -> EndpointId {
        self.router.endpoint().id()
    }

    /// A signed agent card from the host (see [`crate::card`]).
    ///
    /// # Errors
    ///
    /// Returns a message if the card is invalid (empty name, bad lifetime).
    pub fn signed_card(&self, f: &crate::card::Fields, ttl_ms: u64) -> Result<String, String> {
        let ep = self.router.endpoint();
        let sk = ep.secret_key().clone();
        crate::card::build(
            &ep.id().to_string(),
            ep.id().as_bytes(),
            f,
            crate::chat::now_ms(),
            ttl_ms,
            |m| sk.sign(m).to_bytes(),
        )
    }

    /// A launch record for a current member, signed by this host: it says the host started that key with this harness, model and image.
    ///
    /// # Errors
    ///
    /// Returns a message if the member is not in the room or the record is invalid.
    pub fn signed_launch(
        &self,
        member: &EndpointId,
        harness: &str,
        model: &str,
        image: &str,
    ) -> Result<String, String> {
        if !self.members().contains(member) {
            return Err("not a current member".into());
        }
        let ep = self.router.endpoint();
        let sk = ep.secret_key().clone();
        crate::card::build_launch(
            ep.id().as_bytes(),
            member.as_bytes(),
            harness,
            model,
            image,
            crate::chat::now_ms(),
            |m| sk.sign(m).to_bytes(),
        )
    }

    /// The host says something in its own room.
    ///
    /// # Errors
    ///
    /// Returns a message describing why the operation failed.
    pub fn say(&self, text: &str, name: Option<&str>) -> Result<u64, String> {
        self.audit.chat.say("host", name.or(Some("host")), text)
    }

    /// The room's feed as the host sees it (the host is always present).
    #[must_use]
    pub fn feed(&self, since: u64) -> chat::Feed {
        self.audit.chat.touch("host");
        self.feed_ids_since(since)
    }

    /// [`Host::feed`], but when nothing is new wait up to `wait` seconds (capped) for something to arrive.
    pub async fn feed_wait(&self, since: u64, wait: Duration) -> chat::Feed {
        let mut ids: Vec<String> = self
            .members()
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        ids.sort();
        ids.insert(0, "host".into());
        self.audit
            .chat
            .wait_since("host", since, 200, &ids, wait)
            .await
    }

    fn feed_ids_since(&self, since: u64) -> chat::Feed {
        let mut ids: Vec<String> = self
            .members()
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        ids.sort();
        ids.insert(0, "host".into());
        self.audit.chat.since(since, 200, &ids)
    }

    /// How each member was admitted: who holds which grant, with what limits, until when. No tokens.
    #[must_use]
    pub fn admissions(&self) -> Vec<(EndpointId, Admit)> {
        let st = self.state.lock_recover();
        st.admits.iter().map(|(k, v)| (*k, v.clone())).collect()
    }

    /// # Panics
    ///
    /// Panics if the internal lock is poisoned, which only happens after another thread panicked while holding it.
    #[must_use]
    pub fn members(&self) -> Vec<EndpointId> {
        self.state.lock_recover().members.keys().copied().collect()
    }

    /// Revoke a member's grant (and every attenuation of it) and bar the key from rejoining.
    /// Takes effect here on the member's next request, and is signed with the room's root key and
    /// handed to revocation sync so other hosts of the room apply it too.
    /// Returns false for an unknown member, or on a replica (which cannot sign).
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned, which only happens after another thread panicked while holding it.
    #[must_use]
    pub fn revoke_member(&self, id: &EndpointId) -> bool {
        let mut st = self.state.lock_recover();
        let Some(grant_id) = st.members.get(id).cloned() else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
            .unwrap_or(0);
        let notice = revoke::Notice {
            grant_id,
            member: id.to_string(),
            issued_ms: now,
        };
        let Ok(text) = notice.sign(&self.auth) else {
            return false;
        };
        st.apply(&notice, &text);
        st.save();
        drop(st);
        self.audit.record(
            "revoke",
            Some(&id.to_string()),
            json!({ "grant": notice.grant_id.chars().take(8).collect::<String>(), "spread": true }),
        );
        let _ = self.repl.tx.send(text);
        self.roomlog.record("member.revoke", &id.to_string(), now);
        // The member's entries in the room's shared memory go with their access, along with any warm
        // sandbox or persistent session worktree held for them.
        if let Some(mem) = self.memory.clone() {
            let who = id.to_string();
            tokio::spawn(async move {
                mem.forget_author(&who).await;
            });
        }
        {
            let sessions = self.sessions.clone();
            let runner = self.runner.clone();
            let who = *id;
            if let Ok(h) = tokio::runtime::Handle::try_current() {
                h.spawn(async move {
                    drop_member_session(&sessions, runner.as_ref(), who).await;
                });
            }
        }
        let hooks = self.revoke_hooks.lock_recover().clone();
        for hook in hooks {
            hook(id);
        }
        true
    }

    /// Register a callback invoked immediately when [`Host::revoke_member`] revokes a member
    /// (e.g., to drop per-member `BOX-7` egress proxies and revoke scoped `playbox-aiproxy` tokens).
    pub fn on_revoke(&self, hook: impl Fn(&EndpointId) + Send + Sync + 'static) {
        self.revoke_hooks.lock_recover().push(Arc::new(hook));
    }

    /// Leader: start the room's replicated log naming these hosts (endpoint ids; this host is added) and a
    /// threshold `k` of more than half of them. Needs the room's root key, so not on a replica.
    ///
    /// # Errors
    ///
    /// Fails on a replica, if a log exists, or if the host set is invalid.
    pub fn start_room_log(&self, hosts: &[EndpointId], k: usize) -> Result<()> {
        let root = self
            .auth
            .root_signing_key()
            .ok_or_else(|| anyhow!("only the primary can start the room log"))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let mut ids: Vec<[u8; 32]> = hosts.iter().map(|h| *h.as_bytes()).collect();
        ids.push(*self.router.endpoint().id().as_bytes());
        self.roomlog.start(root, ids, k, now)?;
        self.repl
            .allowed
            .lock_recover()
            .extend(hosts.iter().copied());
        for m in self.members() {
            self.roomlog.record("member.join", &m.to_string(), now);
        }
        Ok(())
    }

    /// Leader: have `peers` replay the log and cosign it. Returns how many entries are now committed.
    ///
    /// # Errors
    ///
    /// Fails if this host has no log or is not the leader.
    pub async fn commit_room_log(&self, peers: &[EndpointAddr]) -> Result<u64> {
        let peers = &self.with_log_hosts(peers);
        for p in peers {
            if !p.addrs.is_empty() {
                self.repl.memory.add_endpoint_info(p.clone());
            }
            self.repl.allowed.lock_recover().insert(p.id);
        }
        self.roomlog.commit(self.router.endpoint(), peers).await
    }

    /// Follower: pull the log from the leader and check it.
    ///
    /// # Errors
    ///
    /// Fails if the leader cannot be reached or sends something that does not check.
    pub async fn pull_room_log(&self, leader: &EndpointAddr) -> Result<u64> {
        if !leader.addrs.is_empty() {
            self.repl.memory.add_endpoint_info(leader.clone());
        }
        self.repl.allowed.lock_recover().insert(leader.id);
        self.roomlog.pull(self.router.endpoint(), leader).await
    }

    /// Follower: bid to lead the next epoch because the leader has gone quiet. `peers` are the other hosts; with
    /// `k` signatures (this host's included) it becomes the leader. Returns the new epoch.
    ///
    /// # Errors
    ///
    /// Fails if this host has no log or already leads, or too few hosts signed.
    pub async fn propose_room_log_epoch(&self, peers: &[EndpointAddr]) -> Result<u64> {
        let peers = &self.with_log_hosts(peers);
        for p in peers {
            if !p.addrs.is_empty() {
                self.repl.memory.add_endpoint_info(p.clone());
            }
            self.repl.allowed.lock_recover().insert(p.id);
        }
        self.roomlog.propose(self.router.endpoint(), peers).await
    }

    /// Leader: add and remove hosts (by endpoint id; `add_addrs` helps reach new ones) and set the threshold to
    /// more than half of the result. Needs a quorum of the old set and of the new one. Returns the new epoch.
    ///
    /// # Errors
    ///
    /// Fails if this host has no log or does not lead, or a quorum did not sign.
    pub async fn reconfigure_room_log(
        &self,
        peers: &[EndpointAddr],
        add: &[EndpointAddr],
        remove: &[EndpointId],
    ) -> Result<u64> {
        let mut reach: Vec<EndpointAddr> = self.with_log_hosts(peers);
        for a in add {
            if !reach.iter().any(|p| p.id == a.id) {
                reach.push(a.clone());
            }
        }
        for p in &reach {
            if !p.addrs.is_empty() {
                self.repl.memory.add_endpoint_info(p.clone());
            }
            self.repl.allowed.lock_recover().insert(p.id);
        }
        let ids = |v: &[EndpointId]| v.iter().map(|i| *i.as_bytes()).collect::<Vec<_>>();
        let add_ids: Vec<[u8; 32]> = add.iter().map(|a| *a.id.as_bytes()).collect();
        self.roomlog
            .reconfigure(self.router.endpoint(), &reach, &add_ids, &ids(remove))
            .await
    }

    /// `peers` plus every host the signed log names that is not listed (reached by id alone, so a host added by a
    /// later epoch is found without being listed anywhere).
    fn with_log_hosts(&self, peers: &[EndpointAddr]) -> Vec<EndpointAddr> {
        let mut out = peers.to_vec();
        let me = self.router.endpoint().id();
        if let Some(st) = self.roomlog.status() {
            for h in st.hosts {
                let Some(id) = roomlog_id(&h) else { continue };
                if id != me && !out.iter().any(|p| p.id == id) {
                    out.push(EndpointAddr::new(id));
                }
            }
        }
        out
    }

    /// The program this host runs to measure a candidate for a promotion (see `RoomLogNode::set_evaluator`).
    pub fn set_room_evaluator(&self, program: Option<PathBuf>) {
        self.roomlog.set_evaluator(program);
    }

    /// Leader: register a challenge problem and its promotion rule in the room log.
    ///
    /// # Errors
    ///
    /// Fails if this host does not lead or the rule is refused (`j` under 2, or more than the hosts).
    pub fn register_room_problem(
        &self,
        problem: [u8; 32],
        j: usize,
        noise_ppm: u64,
        min_gain_ppm: u64,
        baseline: i64,
    ) -> Result<()> {
        let reg = playbox_core::promotion::Register {
            problem,
            rule: playbox_core::promotion::Rule {
                j,
                noise_ppm,
                min_gain_ppm,
            },
            baseline,
        };
        self.roomlog.register_problem(&reg, now_ms_host())
    }

    /// Leader: measure `candidate` here and on `peers`, and record a promotion if the problem's rule is met.
    /// Returns the frontier metric.
    ///
    /// # Errors
    ///
    /// Fails if this host does not lead or has no evaluator, or the receipts do not meet the rule.
    pub async fn promote_in_room(
        &self,
        peers: &[EndpointAddr],
        problem: [u8; 32],
        candidate: [u8; 32],
    ) -> Result<i64> {
        let peers = &self.with_log_hosts(peers);
        self.roomlog
            .promote(self.router.endpoint(), peers, problem, candidate)
            .await
    }

    /// Whether this host leads the room log's current epoch.
    #[must_use]
    pub fn room_log_is_leader(&self) -> bool {
        self.roomlog.is_leader()
    }

    /// How long since the room log's leader last asked this host to cosign.
    #[must_use]
    pub fn room_log_silence(&self) -> Duration {
        self.roomlog.silence()
    }

    /// Refuse to sign another host's bid for leader until this host has not heard from the leader for `d`.
    pub fn set_room_log_sticky(&self, d: Duration) {
        self.roomlog.set_sticky(d);
    }

    /// This host's place among the room's hosts, for staggering bids.
    #[must_use]
    pub fn room_log_rank(&self) -> usize {
        self.roomlog.rank()
    }

    /// This host's view of the room log, if it has one.
    #[must_use]
    pub fn room_log_status(&self) -> Option<crate::roomsync::Status> {
        self.roomlog.status()
    }

    /// Where this host's audit log is, if it keeps one in a file.
    pub fn audit_path(&self) -> Option<PathBuf> {
        self.audit.path().map(std::path::Path::to_path_buf)
    }

    /// The room's root public key. Give it to replicas (`Host::open_replica`).
    #[must_use]
    pub fn root_public(&self) -> [u8; 32] {
        self.auth.public_bytes()
    }

    /// Every signed revocation this host knows of.
    ///
    /// # Panics
    ///
    /// Panics only if an internal invariant is broken, which indicates a bug.
    #[must_use]
    pub fn revocation_notices(&self) -> Vec<String> {
        self.state
            .lock_recover()
            .notices
            .values()
            .cloned()
            .collect()
    }

    /// Start spreading revocations with `peers`, the other hosts of this room.
    ///
    /// New notices go out over iroh-gossip; every `interval` this host also asks each peer for all of
    /// theirs, so a host that was offline or missed a broadcast catches up. Peers are trusted only to
    /// *carry* notices (each is checked against the room's root key); only they may pull from this host,
    /// so list this host on theirs too. Call once.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn enable_revocation_sync(
        &self,
        peers: Vec<EndpointAddr>,
        interval: Duration,
    ) -> Result<()> {
        self.repl
            .start(
                self.router.endpoint().clone(),
                self.state.clone(),
                self.auth.clone(),
                peers,
                interval,
            )
            .await
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn shutdown(self) -> Result<()> {
        self.repl.stop();
        drop_all_sessions(&self.sessions, self.runner.as_ref()).await;
        self.router.shutdown().await.map_err(|e| anyhow!("{e}"))
    }
}

fn roomlog_id(hex: &str) -> Option<EndpointId> {
    EndpointId::from_bytes(&playbox_core::roomlog::unhex::<32>(hex)?).ok()
}

fn now_ms_host() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod standing_tests {
    use super::*;

    #[test]
    fn an_expired_grant_has_no_standing() {
        let id = SecretKey::from_bytes(&[7; 32]).public();
        let mut st = State::default();
        st.members.insert(id, "g1".into());
        st.admits.insert(
            id,
            Admit {
                issued: 100,
                expires: 101,
                invite: String::new(),
                cpu_milli: 0,
                mem_mib: 0,
                timeout_secs: 0,
                net: false,
                allowed: vec![],
            },
        );
        assert!(member_standing(&st, &id, 100).is_ok());
        assert!(member_standing(&st, &id, 102).is_err());
        assert!(member_standing(&PositionState::empty(), &id, 100).is_err());
    }

    struct PositionState;
    impl PositionState {
        fn empty() -> State {
            State::default()
        }
    }
}
