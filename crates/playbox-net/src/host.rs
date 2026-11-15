//! The room host: its configuration, how it is started, and what its owner can do with it.

use crate::bind::{bind, Bind};
use crate::grants::Authority;
use crate::handler::RoomHandler;
use crate::keys::load_or_create_seed;
use crate::link::{Link, LinkOpts};
use crate::state::{Invite, State};
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

pub struct Host {
    audit: Arc<audit::Audit>,
    router: Router,
    state: Arc<Mutex<State>>,
    auth: Arc<Authority>,
    transport: Transport,
    repl: Arc<revoke::Replication>,
    memory: Option<Arc<shared_memory::SharedMemory>>,
    caps: Arc<capabilities::Engine>,
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
        Self::build(secret, transport.into(), cfg, Root::Seed(root_seed), state).await
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
        Self::build(
            secret,
            transport.into(),
            cfg,
            Root::Verifier(root_public),
            state,
        )
        .await
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
                if st.banned.contains(&id) {
                    return Err("the member was revoked".into());
                }
                match st.members.get(&id) {
                    None => Err("the member is no longer in the room".into()),
                    Some(grant) if st.revoked.contains(grant) => {
                        Err("the member's grant was revoked".into())
                    }
                    Some(_) => Ok(()),
                }
            })),
        );
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
            latent: Arc::new(latent::Mailbox::new(cfg.latent.clone())),
            caps: caps.clone(),
            convs: Arc::new(conversations::Book::new(cfg.conversations_store.clone())?),
            cfg,
        };
        let sync = revoke::SyncHandler {
            state: state.clone(),
            allowed: repl.allowed.clone(),
        };
        let router = Router::builder(endpoint)
            .accept(ALPN, handler)
            .accept(iroh_gossip::net::GOSSIP_ALPN, gossip)
            .accept(SYNC_ALPN, sync)
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
        })
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
        })
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
        // The member's entries in the room's shared memory go with their access.
        if let Some(mem) = self.memory.clone() {
            let who = id.to_string();
            tokio::spawn(async move {
                mem.forget_author(&who).await;
            });
        }
        true
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
        self.router.shutdown().await.map_err(|e| anyhow!("{e}"))
    }
}
