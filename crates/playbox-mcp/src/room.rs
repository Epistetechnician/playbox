//! Creating, joining, leaving and inspecting a room.

use crate::args::{abuse_from_args, b, latent_from_args, req, s, s_arg, transport_arg};
use crate::{Server, DEFAULT_STEER_IMAGE};
use anyhow::{anyhow, bail, Context, Result};
use playbox_core::{history::text, platform, store};
use playbox_net::{Client, EndpointId, Host, HostConfig, Transport, TransportChoice};
use playbox_runner::{ProcessRunner, Runner};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

impl Server {
    pub(crate) async fn create(&mut self, a: &Value) -> Result<Value> {
        let ttl = a
            .get("invite_ttl_secs")
            .and_then(Value::as_u64)
            .unwrap_or(3600);
        let uses = match a.get("uses").and_then(Value::as_u64) {
            Some(v) => u32::try_from(v)
                .map_err(|_| anyhow!("uses must fit in a 32-bit unsigned integer"))?,
            None => 1,
        };
        if let (Some(have), Some(want)) = (&self.room_name, s(a, "name")) {
            if have != want {
                bail!("this sidecar already hosts the room `{have}`; restart it to host `{want}`");
            }
        }
        if self.host.is_some() && !self.is_replica && s(a, "replica_of").is_some() {
            bail!("this sidecar already hosts a primary room; restart it to open a replica");
        }
        let tpl =
            match s(a, "template") {
                None => None,
                Some(t) => Some(playbox_net::templates::TemplateKind::parse(t).ok_or_else(
                    || anyhow!("unknown template {t:?}; templates are swe, research, review, ops"),
                )?),
            };
        let tpl_spec = tpl.map(playbox_net::templates::TemplateKind::spec);
        if self.host.is_none() {
            let repo = s(a, "repo").map(PathBuf::from);
            if let Some(r) = &repo {
                if !r.join(".git").exists() && !r.join("HEAD").exists() {
                    bail!("{} is not a git repository", r.display());
                }
            }
            let explicit_runtime = s(a, "runtime").or(b(a, "docker").then_some("docker"));
            let runtime = explicit_runtime.or_else(|| tpl_spec.as_ref().map(|s| s.default_runtime));
            let steer_image = s(a, "steer_image")
                .unwrap_or(DEFAULT_STEER_IMAGE)
                .to_string();
            let mut policy = playbox_runner::SandboxPolicy::default();
            if s(a, "steer_store").is_some() {
                policy.allowed_images.push(steer_image.clone());
            }
            let egress_cfg = match a.get("egress") {
                None | Some(Value::Null) => None,
                Some(Value::String(file)) => {
                    Some(playbox_runner::egress::EgressConfig::load(Path::new(file))?)
                }
                Some(v) => Some(playbox_runner::egress::EgressConfig::from_json_bytes(
                    &serde_json::to_vec(v)?,
                )?),
            };
            let allowed_egress: Vec<String> = a
                .get("allowed_egress")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|h| h.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_else(|| {
                    tpl_spec
                        .as_ref()
                        .map(|s| s.limits.allowed_egress.clone())
                        .unwrap_or_default()
                });
            if !allowed_egress.is_empty() {
                let Some(cfg) = &egress_cfg else {
                    bail!("allowed_egress requires `egress` defining the named upstreams");
                };
                for want in &allowed_egress {
                    if !cfg.upstreams.iter().any(|u| &u.name == want) {
                        bail!("allowed_egress {want:?} is not defined in `egress`");
                    }
                }
            }
            let mut actual_runtime = runtime.map(str::to_string);
            let runner: Option<Arc<dyn Runner>> = if b(a, "unsafe_process_runner")
                && playbox_runner::env_opt_in("PLAYBOX_ALLOW_UNSAFE_RUNNER")
            {
                actual_runtime = Some("process".to_string());
                Some(Arc::new(ProcessRunner))
            } else if let Some(name) = runtime {
                let confine = if b(a, "confine") {
                    Some((s(a, "confine_net").unwrap_or("none").parse()?, true))
                } else {
                    None
                };
                let candidate = playbox_runner::runner_with(
                    name,
                    policy,
                    &playbox_runner::RunnerOptions {
                        confine,
                        egress: egress_cfg,
                    },
                )?;
                if explicit_runtime.is_none() && candidate.preflight().await.is_err() {
                    actual_runtime = None;
                    None
                } else {
                    Some(candidate)
                }
            } else {
                None
            };
            // Only the explicit unsafe opt-in lowers the isolation floor.
            let min_isolation = match &runner {
                Some(r) if r.isolation() == playbox_runner::Isolation::None => {
                    playbox_runner::Isolation::None
                }
                _ => HostConfig::default().min_isolation,
            };
            let base_limits = tpl_spec
                .as_ref()
                .map(|s| s.limits.clone())
                .unwrap_or_default();
            let mut limits = playbox_runner::Limits {
                cpus: match a.get("cpus").and_then(Value::as_f64) {
                    Some(v) if !v.is_finite() || v <= 0.0 => {
                        bail!("cpus must be a positive finite number");
                    }
                    Some(v) => v as f32,
                    None => base_limits.cpus,
                },
                mem_mib: match a.get("mem_mib").and_then(Value::as_u64) {
                    Some(v) => u32::try_from(v)
                        .map_err(|_| anyhow!("mem_mib must fit in a 32-bit unsigned integer"))?,
                    None => base_limits.mem_mib,
                },
                timeout_secs: a
                    .get("timeout_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(base_limits.timeout_secs),
                network: a
                    .get("network")
                    .and_then(Value::as_bool)
                    .unwrap_or(base_limits.network),
                allowed_hosts: a
                    .get("allowed_hosts")
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(|h| h.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or(base_limits.allowed_hosts),
                allowed_secrets: a
                    .get("allowed_secrets")
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(|h| h.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or(base_limits.allowed_secrets),
                allowed_egress,
            };
            let steer = match s(a, "steer_store") {
                None => None,
                Some(store) => {
                    if runner.is_none() {
                        bail!("steering needs a runtime: pass runtime=docker (the engine runs in a container with the weights mounted read-only)");
                    }
                    let mem_mib = match a.get("steer_mem_mib").and_then(Value::as_u64) {
                        Some(v) => u32::try_from(v).map_err(|_| {
                            anyhow!("steer_mem_mib must fit in a 32-bit unsigned integer")
                        })?,
                        None => 4096,
                    };
                    let timeout_secs = a
                        .get("steer_timeout_secs")
                        .and_then(Value::as_u64)
                        .unwrap_or(600);
                    // The grant's task caps come from the host's limits, so steering's needs raise them.
                    limits.mem_mib = limits.mem_mib.max(mem_mib);
                    limits.timeout_secs = limits.timeout_secs.max(timeout_secs);
                    let c = a.get("steer_caps").cloned().unwrap_or(Value::Null);
                    let n = |k: &str, d: i64| c.get(k).and_then(Value::as_i64).unwrap_or(d);
                    let list = |k: &str, d: &[&str]| -> Vec<String> {
                        c.get(k)
                            .and_then(Value::as_array)
                            .map(|v| {
                                v.iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_else(|| d.iter().map(|x| x.to_string()).collect())
                    };
                    let caps = playbox_net::grants::SteerCaps {
                        models: list("models", &[]),
                        layer_min: n("layer_min", 0),
                        layer_max: n("layer_max", 255),
                        max_coef_milli: n("max_coef_milli", 4000),
                        modes: list("modes", &["add", "clamp"]),
                        directions: c.get("directions").and_then(Value::as_array).map(|v| {
                            v.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        }),
                        max_tokens_per_run: n("max_tokens_per_run", 4096),
                        classes: list("classes", &[]),
                        probe: c.get("probe").and_then(Value::as_bool).unwrap_or(true),
                        max_runs: u64::try_from(n("max_runs", 50))
                            .map_err(|_| anyhow!("steer_caps.max_runs must be non-negative"))?,
                        max_tokens_total: u64::try_from(n("max_tokens_total", 100_000)).map_err(
                            |_| anyhow!("steer_caps.max_tokens_total must be non-negative"),
                        )?,
                    };
                    let sl = playbox_net::steer::SteerLimits {
                        cpus: limits.cpus,
                        mem_mib,
                        timeout_secs,
                    };
                    Some(Arc::new(playbox_net::steer::SteerHost::open(
                        Path::new(store),
                        &steer_image,
                        sl,
                        caps,
                        playbox_core::steer::Ceiling::LocalInternalExploratory,
                    )?))
                }
            };
            let mut abuse = tpl_spec
                .as_ref()
                .map(|s| s.abuse.clone())
                .unwrap_or_default();
            let parsed_abuse = abuse_from_args(a);
            if a.get("max_members").is_some() {
                abuse.max_members = parsed_abuse.max_members;
            }
            if a.get("max_concurrent").is_some() {
                abuse.max_concurrent_per_member = parsed_abuse.max_concurrent_per_member;
            }
            if a.get("max_tasks_per_minute").is_some() {
                abuse.max_tasks_per_window = parsed_abuse.max_tasks_per_window;
            }
            if a.get("task_budget").is_some() {
                abuse.task_budget = parsed_abuse.task_budget;
            }
            let cfg = HostConfig {
                repo: repo.clone(),
                runner,
                min_isolation,
                limits,
                steer,
                authenticated_patience: a.get("idle_patience_secs").and_then(Value::as_u64).filter(|t| *t > 0).map(Duration::from_secs).unwrap_or(HostConfig::default().authenticated_patience),
                grant_ttl: a.get("grant_ttl_secs").and_then(Value::as_u64).filter(|t| *t > 0).map(Duration::from_secs),
                platform: platform::detect(&store::home()).map_err(|e| anyhow!(e))?.map(std::sync::Arc::from),
                abuse,
                latent: latent_from_args(a),
                capabilities: match a.get("capabilities") {
                    None | Some(Value::Null) => tpl_spec.as_ref().map(|s| s.capabilities.clone()).unwrap_or_default(),
                    Some(v) => serde_json::from_value(v.clone()).context("capabilities must be an array of {name, argv, params, approval, image, timeout_secs}")?,
                },
                ..Default::default()
            };
            let transport = match transport_arg(a)? {
                Some(t) => t,
                None => Transport::from_env()?,
            };
            let name = s(a, "name").unwrap_or("default");
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                bail!("room name may only contain letters, digits, - and _");
            }
            let dir = store::home().join("rooms").join(name);
            let mut cfg = cfg;
            let review = a
                .get("memory_review")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| tpl_spec.as_ref().is_some_and(|s| s.memory_review));
            if let Some(url) = s(a, "memory_url") {
                cfg.memory = Some(playbox_net::shared_memory::hindsight_memory(
                    url,
                    name,
                    review,
                    false,
                    Some(&dir),
                )?);
            } else if a.get("memory") != Some(&Value::Bool(false)) {
                cfg.memory = Some(playbox_net::shared_memory::local_memory(
                    name,
                    review,
                    Some(&dir),
                )?);
            }
            if let Some(endpoint) = s(a, "browse_endpoint") {
                let allow: Vec<String> = a
                    .get("browse_allow")
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut c = playbox_net::browse::BrowseConfig::new(endpoint, allow);
                c.javascript = a
                    .get("browse_javascript")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                cfg.browse = Some(std::sync::Arc::new(playbox_net::browse::Browser::new(c)?));
            }
            let host = match s(a, "replica_of") {
                Some(root) => {
                    let root = root.trim();
                    if root.len() != 64 || !root.is_ascii() {
                        bail!("replica_of must be the room's root public key: 64 hex characters");
                    }
                    let mut key = [0u8; 32];
                    for (i, byte) in key.iter_mut().enumerate() {
                        *byte = u8::from_str_radix(&root[i * 2..i * 2 + 2], 16)?;
                    }
                    Host::open_replica(&dir, transport, cfg, key).await?
                }
                None => Host::open(&dir, transport, cfg).await?,
            };
            if let Some(kind) = tpl {
                playbox_net::templates::seed_room(&host, kind).await;
            }
            self.is_replica = s(a, "replica_of").is_some();
            self.host = Some(Arc::new(host));
            self.host_repo = repo;
            self.room_name = Some(name.to_string());
            self.runtime_name = actual_runtime;
        } else if let Some(kind) = tpl {
            if let Some(host) = &self.host {
                playbox_net::templates::seed_room(host, kind).await;
            }
        }
        let host = self
            .host
            .as_ref()
            .ok_or_else(|| anyhow!("the host did not start"))?;
        let peers: Vec<_> = a
            .get("sync_peers")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(playbox_net::decode_peer)
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        if !peers.is_empty() {
            let every = a
                .get("sync_interval_secs")
                .and_then(Value::as_u64)
                .unwrap_or(15)
                .max(1);
            host.enable_revocation_sync(peers, Duration::from_secs(every))
                .await?;
        }
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let identity = json!({
            "host_id": host.addr().id.to_string(),
            "root_public_key": hex(&host.root_public()),
            "peer_address": playbox_net::encode_peer(&host.addr()),
        });
        if self.is_replica {
            return Ok(json!({
                "replica": true,
                "host_id": identity["host_id"],
                "root_public_key": identity["root_public_key"],
                "peer_address": identity["peer_address"],
                "next": "This host serves the room's grants and applies its revocations, but cannot admit members: members join through the primary and can then use this host with the grant they were given."
            }));
        }
        let bind_to = match s_arg(a, "for_guest") {
            Some(id) => Some(id.trim().parse::<EndpointId>().map_err(|e| anyhow!("for_guest is not a guest id (64 hex characters from the guest's `playbox id`): {e}"))?),
            None => None,
        };
        let link = host.create_link_with(
            Duration::from_secs(ttl),
            uses,
            playbox_net::LinkOpts {
                bind_to,
                show_ips: b(a, "share_ips"),
            },
        )?;
        Ok(json!({
            "root_public_key": identity["root_public_key"],
            "peer_address": identity["peer_address"],
            "link": link.encode(),
            "host_id": host.addr().id.to_string(),
            "transport": link.transport.as_ref().map(Transport::label),
            "bound_to_guest": bind_to.map(|b| b.to_string()),
            "reveals_ip": link.addr.ip_addrs().next().is_some(),
            "valid_secs": ttl,
            "admits": uses,
            "shares_repo": self.host_repo.as_ref().map(|p| p.display().to_string()),
            "members_may_run_tasks": self.runtime_name.is_some(),
            "runtime": self.runtime_name,
            "template": tpl.map(playbox_net::templates::TemplateKind::as_str),
            "policy": host.policy_snapshot(),
            "next": "Send the link to the person whose agent should join. Treat it like a password: anyone holding it can join until it is used or expires."
        }))
    }

    pub(crate) async fn join(&mut self, a: &Value) -> Result<Value> {
        if self.client.is_some() {
            bail!("already joined a room; call playbox_leave first to join another");
        }
        let from_file = match s(a, "link_file") {
            Some(path) => Some(
                std::fs::read_to_string(path)
                    .with_context(|| format!("reading link_file {path}"))?
                    .trim()
                    .to_string(),
            ),
            None => None,
        };
        let link = match (&from_file, s(a, "link")) {
            (Some(l), _) => l.as_str(),
            (None, Some(l)) => l,
            (None, None) => bail!("give `link` or `link_file`"),
        };
        let choice = transport_arg(a)?
            .map(TransportChoice::Use)
            .unwrap_or_default();
        // Check the host before showing it our invite: attestation needs no membership, so a host
        // that fails the check never sees the invite and it stays unspent.
        let parsed = playbox_net::Link::parse(link)?;
        let transport = choice.resolve(parsed.transport.as_ref());
        let client = Client::connect(
            parsed.addr,
            playbox_net::key_for_link(link, &store::home().join("guest.key"))?,
            transport,
        )
        .await?;
        let (host_att, newly_pinned) = self.vet_host(&client, a, "not joining").await?;
        if b(a, "preview") {
            // Nothing has been sent to the host but the attestation request, so the invite is still unspent.
            client.close().await;
            return Ok(json!({
                "joined": false,
                "preview": true,
                "host_attestation": {
                    "verified_tier": host_att.achieved.to_string(),
                    "claimed_tier": host_att.statement.claimed.to_string(),
                    "runner": host_att.statement.measurements.runner,
                    "os": host_att.statement.measurements.os,
                    "notes": host_att.notes,
                    "device": host_att.device,
                },
                "limits": "A grant's limits (cpu, memory, time, network) come with the grant when you join; playbox_status shows them afterwards.",
                "next": "Show this to the person you work for. If they agree, call playbox_join again without `preview`; the invite is unspent."
            }));
        }
        client.join(&parsed.invite).await?;
        // Keep the host's signed statement in our history, so anyone verifying it later can see
        // what we knew about the host when we joined.
        let _ = store::append_kept(
            &self.log_path,
            &self.key,
            "host_attestation",
            host_att.text.as_bytes(),
            store::now_ms(),
        );
        let member = client.id().to_string();
        let ws = match s(a, "workspace") {
            Some(w) => PathBuf::from(w),
            None => store::home()
                .join("workspaces")
                .join(&member[..12.min(member.len())]),
        };
        playbox_git::init(&ws, "main")
            .await
            .context("preparing the workspace")?;
        self.client = Some(Arc::new(client));
        self.hint_hosts = parsed.hosts.clone();
        self.join_args = Some(a.clone());
        self.member_id = Some(member.clone());
        self.workspace = Some(ws.clone());
        self.learn_hosts().await;
        Ok(json!({
            "joined": true,
            "member_id": member,
            "workspace": ws.display().to_string(),
            "host_attestation": {
                "verified_tier": host_att.achieved.to_string(),
                "claimed_tier": host_att.statement.claimed.to_string(),
                "runner": host_att.statement.measurements.runner,
                "binary_blake3": host_att.statement.measurements.binary,
                "policy_blake3": host_att.statement.measurements.policy,
                "os": host_att.statement.measurements.os,
                "notes": host_att.notes,
                "device": host_att.device,
                "device_newly_pinned": newly_pinned,
            }
        }))
    }

    /// Forget the joined room so this sidecar can join another. The grant is not revoked.
    pub(crate) fn leave(&mut self) -> Result<Value> {
        if self.client.is_none() {
            bail!("not in a room");
        }
        self.client = None;
        self.extra.clear();
        self.active = None;
        self.room_root = None;
        self.hint_hosts.clear();
        self.log_hosts.clear();
        self.primary_retry = None;
        self.hosts_checked = None;
        self.member_id = None;
        self.join_args = None;
        self.workspace = None;
        Ok(json!({"left": true}))
    }

    /// Route this guest's calls to another host of the room (a replica), or back to the primary.
    pub(crate) async fn use_host(&mut self, a: &Value) -> Result<Value> {
        let primary = self
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("not in a room: call playbox_join first"))?;
        let host = req(a, "host")?;
        if host == "primary" {
            self.active = None;
            return Ok(json!({"active_host": primary.host_id().to_string(), "which": "primary"}));
        }
        let addr = playbox_net::decode_peer(host)?;
        if addr.addrs.is_empty() {
            bail!("`host` needs a full peer address (from that host's playbox_create), not just its id: an id alone cannot be dialed");
        }
        if addr.id == primary.host_id() {
            self.active = None;
            return Ok(json!({"active_host": addr.id.to_string(), "which": "primary"}));
        }
        if let Some(i) = self.extra.iter().position(|c| c.host_id() == addr.id) {
            self.active = Some(i);
            return Ok(
                json!({"active_host": addr.id.to_string(), "which": "replica", "already_connected": true}),
            );
        }
        let client = primary.sibling(addr).await?;
        let (att, newly_pinned) = self.vet_host(&client, a, "not using this host").await?;
        // The replica must accept our grant before we switch to it.
        client
            .echo("use_host")
            .await
            .map_err(|e| anyhow!("that host did not accept your grant: {e:#}"))?;
        let _ = store::append_kept(
            &self.log_path,
            &self.key,
            "host_attestation",
            att.text.as_bytes(),
            store::now_ms(),
        );
        let id = client.host_id().to_string();
        self.extra.push(Arc::new(client));
        self.active = Some(self.extra.len() - 1);
        Ok(json!({
            "active_host": id,
            "which": "replica",
            "note": "Calls now go to this host. It has its own repository and runner: pull before you delegate or push.",
            "host_attestation": {
                "verified_tier": att.achieved.to_string(),
                "claimed_tier": att.statement.claimed.to_string(),
                "runner": att.statement.measurements.runner,
                "binary_blake3": att.statement.measurements.binary,
                "device": att.device,
                "device_newly_pinned": newly_pinned,
                "notes": att.notes,
            }
        }))
    }

    pub(crate) fn status(&self) -> Result<Value> {
        let hosting = self.host.as_ref().map(|h| {
            json!({
                "repo": self.host_repo.as_ref().map(|p| p.display().to_string()),
                "members": h.members().iter().map(ToString::to_string).collect::<Vec<_>>(),
                // The delegation, host to member: what each grant allows and until when. No tokens, no invites.
                "grants": h.admissions().iter().map(|(k, a)| json!({"member": k.to_string(), "issued": a.issued, "expires": a.expires, "invite": a.invite,
                    "cpu_milli": a.cpu_milli, "mem_mib": a.mem_mib, "timeout_secs": a.timeout_secs, "net": a.net, "allowed": a.allowed})).collect::<Vec<_>>(),
                "replica": self.is_replica,
                "host_id": h.endpoint_id().to_string(),
                "revocations": h.revocation_notices().len(),
                "policy": h.policy_snapshot(),
            })
        });
        let guest = self.client.as_ref().map(|_| {
            json!({
                "member_id": self.member_id,
                "workspace": self.workspace.as_ref().map(|p| p.display().to_string()),
                "active_host": self.guest().ok().map(|c| c.host_id().to_string()),
                "primary_host": self.client.as_ref().map(|c| c.host_id().to_string()),
                "other_hosts": self.extra.iter().map(|c| c.host_id().to_string()).collect::<Vec<_>>(),
            })
        });
        Ok(json!({
            "hosting": hosting,
            "joined": guest,
            "history_log": self.log_path.display().to_string(),
            "history_public_key": text::hex(&self.key.verifying_key().to_bytes()),
            "guest_id": playbox_net::identity_key(&store::home().join("guest.key")).ok().map(|k| k.public().to_string()),
        }))
    }

    pub(crate) async fn policy(&mut self, a: &Value) -> Result<Value> {
        let has_updates = [
            "cpus",
            "mem_mib",
            "timeout_secs",
            "network",
            "allowed_hosts",
            "allowed_secrets",
            "allowed_egress",
            "task_budget",
            "max_members",
            "max_concurrent",
            "max_tasks_per_minute",
            "memory_review",
            "capability_approvals",
        ]
        .iter()
        .any(|k| a.get(*k).is_some() && !a[*k].is_null());
        if has_updates {
            let host = self
                .host
                .as_ref()
                .ok_or_else(|| anyhow!("only the room's host can update live policy guardrails"))?;
            return host.update_policy(a);
        }
        if let Some(host) = &self.host {
            return Ok(host.policy_snapshot());
        }
        self.guest()?.policy().await
    }
}
