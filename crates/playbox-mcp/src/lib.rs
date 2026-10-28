//! A Model Context Protocol server (stdio, JSON-RPC 2.0) that lets any MCP-capable agent host or
//! join a Playbox room. One process is one sidecar: it can host a room, join one as a guest, or both.
//!
//! Every tool call and its result are appended to a signed, hash-chained history log.

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::SigningKey;
use playbox_core::{attest::Tier, history::text, platform, store};
use playbox_net::{Client, EndpointId, Host, HostConfig, Transport, TransportChoice};
use playbox_runner::{ProcessRunner, Runner, TaskSpec};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const FALLBACK_PROTOCOL: &str = "2025-06-18";
const SHOWN_PATCH_BYTES: usize = 20_000;

pub struct Server {
    host: Option<Host>,
    host_repo: Option<PathBuf>,
    room_name: Option<String>,
    runtime_name: Option<String>,
    client: Option<Client>,
    member_id: Option<String>,
    workspace: Option<PathBuf>,
    log_path: PathBuf,
    key: SigningKey,
}

fn tool(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": { "type": "object", "properties": props, "required": required }
    })
}

pub fn tool_list() -> Value {
    json!([
        tool("playbox_create",
            "Start a Playbox room on this machine and return a join link to share. Calling it again returns a fresh single-use link for the same room. Others join with playbox_join.",
            json!({
                "repo": {"type": "string", "description": "Absolute path of a git repository to share with the room"},
                "runtime": {"type": "string", "description": "Let members run tasks, in this sandbox: `libkrun` (a microVM per task, own kernel; needs playbox-krun-helper, see scripts/install-krun.sh) or `docker` (a container, weaker isolation). Both run with no network and limited CPU/memory/time unless the grant says otherwise. Default: members cannot run tasks."},
                "docker": {"type": "boolean", "description": "Shorthand for runtime=docker."},
                "transport": {"type": "string", "description": "How peers reach each other: `iroh` (public relays + discovery, works across NATs; default), `local` (same machine/LAN only), or `relay:<https url>` (your own iroh-relay; set PLAYBOX_RELAY_CA to a PEM file for a private CA). Default can be set with PLAYBOX_TRANSPORT."},
                "local": {"type": "boolean", "description": "Shorthand for transport=local."},
                "name": {"type": "string", "description": "Room name. The room's identity, members, bans and unspent invites are kept under this name, so creating it again after a restart brings the same room back. Default: default."},
                "invite_ttl_secs": {"type": "integer", "description": "How long the link works. Default 3600."},
                "uses": {"type": "integer", "description": "How many members the link admits. Default 1."}
            }), &[]),
        tool("playbox_join",
            "Join a Playbox room using a link someone shared (starts with playbox://). Gives this agent a scoped, expiring grant.",
            json!({
                "link": {"type": "string"},
                "workspace": {"type": "string", "description": "Directory for this room's git clone. Default: under the Playbox home."},
                "min_host_tier": {"type": "string", "description": "Refuse to join unless the host's attestation achieves at least this tier (t0..t3). Default t0: the host must sign measurements of its software with its connection key. t1 needs hardware evidence (Secure Enclave or TPM) from a device key you have pinned (`playbox pin add`, or trust_host_device). t2 also needs PLAYBOX_TPM_BOOT_PCRS reference values."},
                "trust_host_device": {"type": "boolean", "description": "Pin the host's device key if it is new to you (trust on first use), then judge the host with it pinned. Only do this when you have good reason to think you are talking to the host you meant: the first contact is the one an impostor could win."},
                "transport": {"type": "string", "description": "Override the transport (`iroh`, `local`, `relay:<url>`). By default the link says which to use."},
                "local": {"type": "boolean", "description": "Shorthand for transport=local."}
            }), &["link"]),
        tool("playbox_status",
            "Show whether this sidecar hosts a room and/or has joined one, who the members are, and where the history log is.",
            json!({}), &[]),
        tool("playbox_ping",
            "Send a short text to the room host and get it echoed back. Use it to check the connection and your grant.",
            json!({"text": {"type": "string"}}), &["text"]),
        tool("playbox_pull",
            "Fetch a branch or commit from the room's shared repository into your workspace as refs/remotes/playbox/<ref>.",
            json!({
                "ref": {"type": "string", "description": "Branch name, e.g. main"},
                "checkout": {"type": "boolean", "description": "Also check it out as a local branch of the same name (resets that branch). Default false."}
            }), &["ref"]),
        tool("playbox_delegate",
            "Run a shell command on the room host, in a sandbox, against a checkout of a commit. Returns output and a patch of everything the command changed. Subject to your grant's limits (cpu, memory, time, network).",
            json!({
                "base": {"type": "string", "description": "Branch or commit the host should check out, e.g. main"},
                "command": {"type": "string", "description": "Run with sh -c inside the sandbox"},
                "image": {"type": "string", "description": "Container image. Default alpine:3"},
                "cpus": {"type": "number"}, "mem_mib": {"type": "integer"},
                "timeout_secs": {"type": "integer"}, "network": {"type": "boolean"},
                "apply": {"type": "boolean", "description": "Apply the returned patch to your workspace (staged, not committed). Default false."}
            }), &["base", "command"]),
        tool("playbox_push",
            "Send a branch from your workspace to the room. It is stored under your own namespace on the host and never overwrites the host's branches.",
            json!({"branch": {"type": "string"}}), &["branch"]),
        tool("playbox_verify",
            "Verify a history log: hash chain, signatures, and any attestations in it (including the host's, recorded when you joined). Defaults to this session's log. Verified tier T0 means the key holder signed the claims; higher tiers need platform verifiers that are not available yet.",
            json!({"path": {"type": "string"}}), &[]),
        tool("playbox_revoke",
            "Host only: revoke a member's grant and bar their key from rejoining.",
            json!({"member": {"type": "string", "description": "Member id as shown by playbox_status"}}), &["member"]),
    ])
}

fn s_arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn s<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn req<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    s(a, k).ok_or_else(|| anyhow!("missing required argument `{k}`"))
}

fn b(a: &Value, k: &str) -> bool {
    a.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn transport_arg(a: &Value) -> Result<Option<Transport>> {
    if let Some(t) = s(a, "transport") {
        return Ok(Some(Transport::parse(t)?));
    }
    Ok(b(a, "local").then_some(Transport::Local))
}

fn ok_text(v: &Value) -> Value {
    json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(v).unwrap_or_default()}], "isError": false})
}

fn err_text(msg: &str) -> Value {
    json!({"content": [{"type": "text", "text": msg}], "isError": true})
}

impl Server {
    pub fn new(home: &Path) -> Result<Self> {
        let key = store::load_or_create_key(&home.join("session.key"))?;
        let log_path = std::env::var_os("PLAYBOX_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("history").join(format!("mcp-{}.log", store::now_ms())));
        Ok(Self { host: None, host_repo: None, room_name: None, runtime_name: None, client: None, member_id: None, workspace: None, log_path, key })
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    fn record(&self, kind: &str, payload: &[u8]) {
        if let Err(e) = store::append(&self.log_path, &self.key, kind, payload, store::now_ms()) {
            eprintln!("playbox-mcp: could not record history: {e}");
        }
    }

    /// Handle one JSON-RPC message. Returns the response, or `None` for notifications.
    pub async fn handle(&mut self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": s(&params, "protocolVersion").unwrap_or(FALLBACK_PROTOCOL),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "playbox-mcp", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Playbox lets agents from different owners collaborate. Use playbox_create to host a room and share the link, or playbox_join to enter one."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_list()})),
            "tools/call" => {
                let name = s(&params, "name").unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                Ok(self.call(&name, &args).await)
            }
            _ if id.is_none() => return None, // unknown notification
            _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
        };
        let id = id?;
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        })
    }

    async fn call(&mut self, name: &str, args: &Value) -> Value {
        self.record("tool_call", json!({"tool": name, "args": args}).to_string().as_bytes());
        let out = match name {
            "playbox_create" => self.create(args).await,
            "playbox_join" => self.join(args).await,
            "playbox_status" => self.status(),
            "playbox_ping" => self.ping(args).await,
            "playbox_pull" => self.pull(args).await,
            "playbox_delegate" => self.delegate(args).await,
            "playbox_push" => self.push(args).await,
            "playbox_verify" => self.verify(args),
            "playbox_revoke" => self.revoke(args),
            other => Err(anyhow!("unknown tool: {other}")),
        };
        let result = match out {
            Ok(v) => ok_text(&v),
            Err(e) => err_text(&format!("{e:#}")),
        };
        self.record("tool_result", result.to_string().as_bytes());
        result
    }

    fn guest(&self) -> Result<&Client> {
        self.client.as_ref().ok_or_else(|| anyhow!("not in a room: call playbox_join first"))
    }

    fn workspace(&self) -> Result<&Path> {
        self.workspace.as_deref().ok_or_else(|| anyhow!("not in a room: call playbox_join first"))
    }

    async fn create(&mut self, a: &Value) -> Result<Value> {
        let ttl = a.get("invite_ttl_secs").and_then(Value::as_u64).unwrap_or(3600);
        let uses = a.get("uses").and_then(Value::as_u64).unwrap_or(1) as u32;
        if let (Some(have), Some(want)) = (&self.room_name, s(a, "name")) {
            if have != want {
                bail!("this sidecar already hosts the room `{have}`; restart it to host `{want}`");
            }
        }
        if self.host.is_none() {
            let repo = s(a, "repo").map(PathBuf::from);
            if let Some(r) = &repo {
                if !r.join(".git").exists() && !r.join("HEAD").exists() {
                    bail!("{} is not a git repository", r.display());
                }
            }
            let runtime = s(a, "runtime").or(b(a, "docker").then_some("docker"));
            let runner: Option<Arc<dyn Runner>> = if let Some(name) = runtime {
                Some(playbox_runner::runner_by_name(name, playbox_runner::SandboxPolicy::default())?)
            } else if b(a, "unsafe_process_runner") && std::env::var_os("PLAYBOX_ALLOW_UNSAFE_RUNNER").is_some() {
                Some(Arc::new(ProcessRunner))
            } else {
                None
            };
            // Only the explicit unsafe opt-in lowers the isolation floor.
            let min_isolation = match &runner {
                Some(r) if r.isolation() == playbox_runner::Isolation::None => playbox_runner::Isolation::None,
                _ => HostConfig::default().min_isolation,
            };
            let cfg = HostConfig {
                repo: repo.clone(),
                runner,
                min_isolation,
                platform: platform::detect(&store::home()).map_err(|e| anyhow!(e))?.map(std::sync::Arc::from),
                ..Default::default()
            };
            let transport = match transport_arg(a)? {
                Some(t) => t,
                None => Transport::from_env()?,
            };
            let name = s(a, "name").unwrap_or("default");
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                bail!("room name may only contain letters, digits, - and _");
            }
            let dir = store::home().join("rooms").join(name);
            self.host = Some(Host::open(&dir, transport, cfg).await?);
            self.host_repo = repo;
            self.room_name = Some(name.to_string());
            self.runtime_name = runtime.map(str::to_string);
        }
        let host = self.host.as_ref().expect("host started");
        let link = host.create_link(Duration::from_secs(ttl), uses)?;
        Ok(json!({
            "link": link.encode(),
            "host_id": host.addr().id.to_string(),
            "transport": link.transport.as_ref().map(Transport::label),
            "valid_secs": ttl,
            "admits": uses,
            "shares_repo": self.host_repo.as_ref().map(|p| p.display().to_string()),
            "members_may_run_tasks": self.runtime_name.is_some(),
            "runtime": self.runtime_name,
            "next": "Send the link to the person whose agent should join. Treat it like a password: anyone holding it can join until it is used or expires."
        }))
    }

    async fn join(&mut self, a: &Value) -> Result<Value> {
        if self.client.is_some() {
            bail!("already joined a room; restart this sidecar to join another");
        }
        let link = req(a, "link")?;
        let choice = transport_arg(a)?.map(TransportChoice::Use).unwrap_or_default();
        let min = Tier::parse(s_arg(a, "min_host_tier").unwrap_or("t0")).map_err(|e| anyhow!(e))?;
        // Check the host before showing it our invite: attestation needs no membership, so a host
        // that fails the check never sees the invite and it stays unspent.
        let parsed = playbox_net::Link::parse(link)?;
        let transport = choice.resolve(parsed.transport.as_ref());
        let client = Client::connect(parsed.addr, playbox_net::new_secret_key()?, transport).await?;
        let pins_path = platform::pins_path(&store::home());
        let mut pins = platform::PinStore::load(&pins_path)?;
        let boot = platform::boot_reference_from_env().map_err(|e| anyhow!(e))?;
        let make = |pins: &platform::PinStore| platform::verifiers(pins.clone(), boot.clone()).map_err(|e| anyhow!(e));
        let mut vs = make(&pins)?;
        let mut host_att = {
            let refs: Vec<&dyn playbox_core::attest::PlatformVerifier> = vs.iter().map(|v| v.as_ref()).collect();
            match client.verify_host(Tier::T0, &refs).await {
                Ok(v) => v,
                Err(e) => bail!("not joining: {e:#}"),
            }
        };
        let mut newly_pinned = false;
        if b(a, "trust_host_device") {
            if let Some((kind, fp)) = host_att.device.clone().as_deref().and_then(|d| d.split_once(':')) {
                if !pins.contains(kind, fp) {
                    pins.add(kind, fp)?;
                    newly_pinned = true;
                    vs = make(&pins)?;
                    let refs: Vec<&dyn playbox_core::attest::PlatformVerifier> = vs.iter().map(|v| v.as_ref()).collect();
                    host_att = match client.verify_host(Tier::T0, &refs).await {
                        Ok(v) => v,
                        Err(e) => bail!("not joining: {e:#}"),
                    };
                }
            }
        }
        if host_att.achieved < min {
            bail!("not joining: host achieves {} but {} was required ({})", host_att.achieved, min, host_att.notes.join("; "));
        }
        client.join(&parsed.invite).await?;
        // Keep the host's signed statement in our history, so anyone verifying it later can see
        // what we knew about the host when we joined.
        let _ = store::append_kept(&self.log_path, &self.key, "host_attestation", host_att.text.as_bytes(), store::now_ms());
        let member = client.id().to_string();
        let ws = match s(a, "workspace") {
            Some(w) => PathBuf::from(w),
            None => store::home().join("workspaces").join(&member[..12.min(member.len())]),
        };
        tokio::fs::create_dir_all(&ws).await?;
        if !ws.join(".git").exists() {
            let out = tokio::process::Command::new("git").arg("-C").arg(&ws).args(["init", "-q", "-b", "main"]).output().await?;
            if !out.status.success() {
                bail!("git init failed: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
        self.client = Some(client);
        self.member_id = Some(member.clone());
        self.workspace = Some(ws.clone());
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

    fn status(&self) -> Result<Value> {
        let hosting = self.host.as_ref().map(|h| {
            json!({
                "repo": self.host_repo.as_ref().map(|p| p.display().to_string()),
                "members": h.members().iter().map(ToString::to_string).collect::<Vec<_>>(),
            })
        });
        let guest = self.client.as_ref().map(|_| {
            json!({"member_id": self.member_id, "workspace": self.workspace.as_ref().map(|p| p.display().to_string())})
        });
        Ok(json!({
            "hosting": hosting,
            "joined": guest,
            "history_log": self.log_path.display().to_string(),
            "history_public_key": text::hex(&self.key.verifying_key().to_bytes()),
        }))
    }

    async fn ping(&self, a: &Value) -> Result<Value> {
        Ok(json!({"echo": self.guest()?.echo(req(a, "text")?).await?}))
    }

    async fn pull(&self, a: &Value) -> Result<Value> {
        let r = req(a, "ref")?;
        let ws = self.workspace()?;
        let (head, bundle) = self.guest()?.pull(r).await?;
        let dest = format!("refs/remotes/playbox/{r}");
        let src = format!("refs/heads/{r}");
        let got = match playbox_git::bundle_import(ws, &bundle, &src, &dest).await {
            Ok(h) => h,
            // The ref may be a tag or commit id rather than a branch.
            Err(_) => playbox_git::bundle_import(ws, &bundle, r, &dest).await?,
        };
        if b(a, "checkout") {
            let out = tokio::process::Command::new("git")
                .arg("-C").arg(ws).args(["checkout", "-q", "-B", r, &dest]).output().await?;
            if !out.status.success() {
                bail!("checkout failed: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
        Ok(json!({"ref": dest, "head": got, "host_head": head, "bytes": bundle.len(), "checked_out": b(a, "checkout")}))
    }

    async fn delegate(&self, a: &Value) -> Result<Value> {
        let base = req(a, "base")?;
        let command = req(a, "command")?;
        let mut spec = TaskSpec::new(&["sh", "-c", command]);
        if let Some(i) = s(a, "image") {
            spec.image = i.to_string();
        }
        if let Some(v) = a.get("cpus").and_then(Value::as_f64) {
            spec.cpus = v as f32;
        }
        if let Some(v) = a.get("mem_mib").and_then(Value::as_u64) {
            spec.mem_mib = v as u32;
        }
        if let Some(v) = a.get("timeout_secs").and_then(Value::as_u64) {
            spec.timeout_secs = v;
        }
        spec.network = b(a, "network");
        let run = self.guest()?.run_task_full(base, spec).await?;
        let (runner, out, patch) = (run.runner.clone(), run.output.clone(), run.patch.clone());
        let mut applied = false;
        if b(a, "apply") && !patch.is_empty() {
            playbox_git::apply_patch(self.workspace()?, &patch).await.context("applying patch")?;
            applied = true;
        }
        let shown = String::from_utf8_lossy(&patch[..patch.len().min(SHOWN_PATCH_BYTES)]).into_owned();
        Ok(json!({
            "runner": runner,
            "exit_code": out.exit_code,
            "timed_out": out.timed_out,
            "stdout": out.stdout,
            "stderr": out.stderr,
            "patch_bytes": patch.len(),
            "patch": shown,
            "patch_truncated": patch.len() > SHOWN_PATCH_BYTES,
            "patch_applied": applied,
            "task_id": run.task_id,
            "host_branch": run.branch,
            "host_commit": run.commit,
            "changed": run.changed,
        }))
    }

    async fn push(&self, a: &Value) -> Result<Value> {
        let branch = req(a, "branch")?;
        let bundle = playbox_git::bundle_create(self.workspace()?, branch).await?;
        let reference = self.guest()?.push(branch, &bundle).await?;
        Ok(json!({"stored_as": reference, "bytes": bundle.len()}))
    }

    fn verify(&self, a: &Value) -> Result<Value> {
        let path = s(a, "path").map(PathBuf::from).unwrap_or_else(|| self.log_path.clone());
        let r = store::verify_deep(&path, &[])?;
        let atts: Vec<Value> = r
            .attestations
            .iter()
            .map(|x| json!({"entry": x.seq, "about": x.about, "subject": x.subject, "claimed": x.claimed.to_string(), "verified": x.achieved.to_string(), "notes": x.notes}))
            .collect();
        Ok(json!({
            "valid": r.ok(),
            "entries": r.entries,
            "head": r.head,
            "path": path.display().to_string(),
            "error": r.chain_error,
            "problems": r.problems,
            "attestations": atts,
        }))
    }

    fn revoke(&self, a: &Value) -> Result<Value> {
        let host = self.host.as_ref().ok_or_else(|| anyhow!("this sidecar is not hosting a room"))?;
        let id: EndpointId = req(a, "member")?.parse().map_err(|e| anyhow!("not a member id: {e}"))?;
        Ok(json!({"revoked": host.revoke_member(&id)}))
    }
}
