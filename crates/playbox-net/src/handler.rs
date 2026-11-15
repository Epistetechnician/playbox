//! The host side of one connection: admission, authorization and the room's operations.

use crate::grants::{self, Authority, Caps};
use crate::host::HostConfig;
use crate::state::State;
use crate::wire::{self, Op};
use crate::{
    audit, browse, capabilities, conversations, latent, limits, MAX_FRAME,
    MAX_UNAUTHENTICATED_REQUEST, MAX_UNAUTHENTICATED_REQUESTS, TASKS_KEPT,
};
use a2a::{
    A2AError, GetTaskRequest, JsonRpcError, JsonRpcId, JsonRpcResponse, ListTasksResponse, Message,
    Part, Role, SendMessageRequest, SendMessageResponse, Task, TaskState,
};
use anyhow::{anyhow, bail, Result};
use ed25519_dalek::SigningKey;
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
    EndpointId,
};
use playbox_core::attest::{self, Measurements, PlatformProvider};
use playbox_runner::TaskSpec;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub(crate) struct RoomHandler {
    pub(crate) state: Arc<Mutex<State>>,
    pub(crate) cfg: HostConfig,
    pub(crate) tasks: Arc<Semaphore>,
    pub(crate) limiter: limits::Limiter,
    pub(crate) conns: Arc<Semaphore>,
    pub(crate) audit: Arc<audit::Audit>,
    pub(crate) peer_conns: Arc<Mutex<HashMap<EndpointId, usize>>>,
    pub(crate) pushed: Arc<Mutex<HashMap<EndpointId, u64>>>,
    /// Bounds concurrent calls into platform hardware, which anyone may trigger without a grant.
    pub(crate) platform_calls: Arc<Semaphore>,
    pub(crate) auth: Arc<Authority>,
    /// Recent tasks per member, with byte payloads dropped. Not persisted.
    pub(crate) done: Arc<Mutex<HashMap<EndpointId, VecDeque<Task>>>>,
    pub(crate) card: Arc<Value>,
    /// Signs attestation statements with the host's endpoint key, so a client can bind them to the
    /// connection it already authenticated.
    pub(crate) attest_key: Arc<SigningKey>,
    pub(crate) measurements: Arc<Measurements>,
    pub(crate) platform: Option<Arc<dyn PlatformProvider>>,
    /// Signed hub latent messages posted to the room, kept in `<state dir>/latent.json` when the host has one.
    pub(crate) latent: Arc<latent::Mailbox>,
    pub(crate) caps: Arc<capabilities::Engine>,
    pub(crate) convs: Arc<conversations::Book>,
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

pub(crate) fn refuse(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Refused(msg.into()))
}

fn rpc_error(code: i32, message: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code,
        message: message.into(),
        data: None,
    }
}

pub(crate) fn unauthorized(e: anyhow::Error) -> JsonRpcError {
    rpc_error(wire::ERR_UNAUTHORIZED, format!("{e:#}"))
}

/// A task for a result: success, or Rejected / Failed according to the error.
pub(crate) fn finish(result: Result<Task>) -> Task {
    match result {
        Ok(t) => t,
        Err(e) => {
            let state = if e.downcast_ref::<Refused>().is_some() {
                TaskState::Rejected
            } else {
                TaskState::Failed
            };
            wire::task(state, Some(format!("{e:#}")), vec![], vec![])
        }
    }
}

impl RoomHandler {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "the value is bounded, or truncated on purpose; the value is bounded well below the signed maximum"
    )]
    fn join(&self, from: EndpointId, invite: String) -> Result<Value> {
        if !self.auth.can_issue() {
            bail!("this host is a replica and cannot admit members; join through the room's primary host");
        }
        if self
            .state
            .lock()
            .expect("state lock")
            .banned
            .contains(&from)
        {
            bail!("this key is banned from the room");
        }
        let id = self.auth.verify_invite(&invite)?;
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if st.members.len() >= self.limiter.max_members() && !st.members.contains_key(&from) {
            bail!("this room is full ({} members)", self.limiter.max_members());
        }
        let inv = st
            .invites
            .get_mut(&id)
            .ok_or_else(|| anyhow!("invite already used or unknown"))?;
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
            mem_mib: i64::from(l.mem_mib),
            timeout_secs: i64::try_from(l.timeout_secs).unwrap_or(i64::MAX),
            net: l.network,
            // A secret is granted as `secret/NAME` in the same allow-list as hosts: `/` is in no host name or secret name.
            hosts: l
                .allowed_hosts
                .iter()
                .cloned()
                .chain(l.allowed_secrets.iter().map(|n| format!("secret/{n}")))
                .chain(
                    self.cfg
                        .capabilities
                        .iter()
                        .map(|c| format!("cap/{}", c.name)),
                )
                .collect(),
        };
        let ttl = self
            .cfg
            .grant_ttl
            .unwrap_or(Duration::from_secs(3600))
            .as_secs() as i64;
        let (grant, grant_id) = self.auth.mint_grant_with(
            &from.to_string(),
            ttl,
            &caps,
            self.cfg.steer.as_ref().map(|h| h.caps()),
        )?;
        st.members.insert(from, grant_id);
        st.save();
        Ok(
            json!({ "member": from.to_string(), "grant": grant, "protocol": wire::PROTOCOL, "version": env!("CARGO_PKG_VERSION") }),
        )
    }

    pub(crate) fn authorize(
        &self,
        grant: Option<&str>,
        from: EndpointId,
        op: &str,
        needs: &grants::Needs,
    ) -> Result<(), JsonRpcError> {
        let Some(grant) = grant else {
            return Err(unauthorized(anyhow!("no grant presented")));
        };
        let revoked = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revoked
            .clone();
        self.auth
            .authorize(grant, &from.to_string(), op, needs, &revoked)
            .map_err(unauthorized)
    }

    /// One JSON-RPC call from `from`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "the value is bounded, or truncated on purpose; the value is bounded well below the signed maximum"
    )]
    async fn route(&self, from: EndpointId, frame: &wire::Frame) -> Result<Value, JsonRpcError> {
        use a2a::methods;
        let rpc = &frame.rpc;
        let params = rpc.params.clone().unwrap_or(Value::Null);
        let invalid =
            |e: serde_json::Error| A2AError::invalid_params(e.to_string()).to_jsonrpc_error();
        match rpc.method.as_str() {
            wire::METHOD_AGENT_CARD => Ok((*self.card).clone()),
            wire::METHOD_ACTIVATE => {
                let Some(p) = self.platform.clone() else {
                    return Err(A2AError::unsupported_operation(
                        "this host has no platform hardware",
                    )
                    .to_jsonrpc_error());
                };
                let req = params
                    .get("request")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if req.len() > 16384
                    || !req.len().is_multiple_of(2)
                    || !req.chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(
                        A2AError::invalid_params("`request` must be at most 8 KiB of hex")
                            .to_jsonrpc_error(),
                    );
                }
                let bytes: Vec<u8> = (0..req.len() / 2)
                    .map(|i| u8::from_str_radix(&req[i * 2..i * 2 + 2], 16).unwrap_or(0))
                    .collect();
                let _slot = self.platform_calls.try_acquire().map_err(|_| {
                    A2AError::unsupported_operation(
                        "busy: too many hardware challenges in flight, try again",
                    )
                    .to_jsonrpc_error()
                })?;
                let out = tokio::task::spawn_blocking(move || p.respond(&bytes))
                    .await
                    .map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())?;
                match out {
                    Ok(r) => Ok(
                        json!({ "response": r.iter().map(|b| format!("{b:02x}")).collect::<String>() }),
                    ),
                    Err(e) => Err(rpc_error(
                        wire::ERR_UNAUTHORIZED,
                        format!("challenge failed: {e}"),
                    )),
                }
            }
            wire::METHOD_ATTESTATION => {
                let nonce = params
                    .get("nonce")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if nonce.len() > 64 || !nonce.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(A2AError::invalid_params(
                        "`nonce` must be at most 64 hex characters",
                    )
                    .to_jsonrpc_error());
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
                    .unwrap_or(0);
                let plain = || {
                    attest::new_statement(
                        &self.attest_key,
                        nonce,
                        "",
                        now,
                        (*self.measurements).clone(),
                    )
                    .sign(&self.attest_key)
                    .map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())
                };
                let text = match &self.platform {
                    None => plain()?,
                    Some(p) => {
                        // Anyone may ask for this without a grant and each one drives the hardware, so bound
                        // how many run at once and tell the rest to come back.
                        let _slot = self.platform_calls.try_acquire().map_err(|_| {
                            A2AError::unsupported_operation(
                                "busy: too many attestations in flight, try again",
                            )
                            .to_jsonrpc_error()
                        })?;
                        // Talking to hardware blocks (a TPM quote takes a moment), so keep it off the runtime threads.
                        let (key, m, p, nonce) = (
                            self.attest_key.clone(),
                            (*self.measurements).clone(),
                            p.clone(),
                            nonce.to_string(),
                        );
                        let made = tokio::task::spawn_blocking(move || {
                            attest::new_platform_statement(&key, &nonce, "", now, m, p.as_ref())
                        })
                        .await;
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
                let invite = params
                    .get("invite")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
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
                let op = Op::from_message(&req.message)
                    .map_err(|e| A2AError::invalid_params(format!("{e:#}")).to_jsonrpc_error())?;
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
                    return self
                        .steer_message(from, frame.grant(), op.expect("matched Some"), &req.message)
                        .await;
                }
                if let Some(op @ Op::Handoff { .. }) = op {
                    return self.handoff_message(from, op, &req.message).await;
                }
                let reply = self.send_message(from, op, &req.message).await;
                serde_json::to_value(reply)
                    .map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())
            }
            methods::GET_TASK => {
                self.authorize(frame.grant(), from, "tasks", &grants::Needs::default())?;
                let req: GetTaskRequest = serde_json::from_value(params).map_err(invalid)?;
                let t = self
                    .find_task(from, &req.id)
                    .ok_or_else(|| A2AError::task_not_found(&req.id).to_jsonrpc_error())?;
                serde_json::to_value(t)
                    .map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())
            }
            methods::LIST_TASKS => {
                self.authorize(frame.grant(), from, "tasks", &grants::Needs::default())?;
                let tasks: Vec<Task> = self
                    .done
                    .lock()
                    .expect("tasks lock")
                    .get(&from)
                    .map(|q| q.iter().cloned().collect())
                    .unwrap_or_default();
                let n = tasks.len() as i32;
                let resp = ListTasksResponse {
                    tasks,
                    next_page_token: String::new(),
                    page_size: n,
                    total_size: n,
                };
                serde_json::to_value(resp)
                    .map_err(|e| A2AError::internal(e.to_string()).to_jsonrpc_error())
            }
            methods::CANCEL_TASK => {
                self.authorize(frame.grant(), from, "tasks", &grants::Needs::default())?;
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                // Every task here finishes before its response is sent, so none can still be cancelled.
                match self.find_task(from, &id) {
                    Some(_) => Err(A2AError::task_not_cancelable(&id).to_jsonrpc_error()),
                    None => Err(A2AError::task_not_found(&id).to_jsonrpc_error()),
                }
            }
            m if methods::is_streaming(m) => Err(A2AError::unsupported_operation(
                "streaming is not supported by this room",
            )
            .to_jsonrpc_error()),
            methods::GET_EXTENDED_AGENT_CARD => {
                Err(A2AError::extended_card_not_configured().to_jsonrpc_error())
            }
            m if methods::is_valid(m) => Err(A2AError::unsupported_operation(format!(
                "{m} is not supported by this room"
            ))
            .to_jsonrpc_error()),
            m => Err(rpc_error(
                a2a::error_code::METHOD_NOT_FOUND,
                format!("method not found: {m}"),
            )),
        }
    }

    fn find_task(&self, from: EndpointId, id: &str) -> Option<Task> {
        self.done
            .lock()
            .expect("tasks lock")
            .get(&from)?
            .iter()
            .find(|t| t.id == id)
            .cloned()
    }

    pub(crate) fn remember(&self, from: EndpointId, t: &Task) {
        let mut done = self
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let q = done.entry(from).or_default();
        q.push_back(wire::without_bytes(t.clone()));
        while q.len() > TASKS_KEPT {
            q.pop_front();
        }
    }

    async fn send_message(
        &self,
        from: EndpointId,
        op: Option<Op>,
        msg: &Message,
    ) -> SendMessageResponse {
        let reply = |text: String| {
            SendMessageResponse::Message(Message::new(Role::Agent, vec![Part::text(text)]))
        };
        let task = match op {
            None => {
                return reply(
                    "Playbox room. Send a data part like {\"playbox\":{\"op\":\"echo\",\"text\":\"hi\"}}; \
                     see the skills on the agent card (playbox.GetAgentCard)."
                        .into(),
                )
            }
            Some(Op::Echo { text }) => return reply(text),
            Some(Op::Say { text, name, request_id }) => {
                return match self.audit.chat.say_once(&from.to_string(), name.as_deref(), &text, request_id.as_deref()) {
                    Ok(seq) => reply(json!({ "seq": seq }).to_string()),
                    Err(e) => reply(json!({ "error": e }).to_string()),
                }
            }
            Some(Op::Remember { text, key }) => {
                let t0 = std::time::Instant::now();
                let Some(mem) = self.cfg.memory.clone() else { return reply(json!({ "error": "this room has no shared memory" }).to_string()) };
                let r = mem.remember(&from.to_string(), &text, key.as_deref()).await;
                self.audit.record(
                    "remember",
                    Some(&from.to_string()),
                    json!({ "ok": r.is_ok(), "bytes": text.len(), "ms": t0.elapsed().as_millis() as u64, "doc": r.as_ref().ok().map(|x| x.doc.clone()), "reason": r.as_ref().err().map(|e| audit::clip(e)) }),
                );
                return match r {
                    Ok(x) => reply(json!({ "doc": x.doc, "status": x.status }).to_string()),
                    Err(e) => reply(json!({ "error": e }).to_string()),
                };
            }
            Some(Op::Recall { query, max_tokens, author }) => {
                let t0 = std::time::Instant::now();
                let Some(mem) = self.cfg.memory.clone() else { return reply(json!({ "error": "this room has no shared memory" }).to_string()) };
                let members: std::collections::HashSet<String> = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).members.keys().map(std::string::ToString::to_string).collect();
                let r = mem.recall(&query, max_tokens, author.as_deref(), &|k: &str| members.contains(k)).await;
                self.audit.record(
                    "recall",
                    Some(&from.to_string()),
                    json!({ "ok": r.is_ok(), "hits": r.as_ref().ok().map(Vec::len), "ms": t0.elapsed().as_millis() as u64, "reason": r.as_ref().err().map(|e| audit::clip(e)) }),
                );
                return match r {
                    Ok(hits) => reply(json!({ "hits": hits }).to_string()),
                    Err(e) => reply(json!({ "error": e }).to_string()),
                };
            }
            Some(Op::ConvList) => return reply(json!({ "conversations": self.convs.list(&from.to_string()) }).to_string()),
            Some(Op::ConvNew { title, members }) => {
                let me = from.to_string();
                let known = self.member_ids();
                let r = self.convs.create(&me, &title, &members, &|m: &str| known.contains(m));
                if let Ok(c) = &r {
                    self.audit.record("conv_new", Some(&me), json!({ "conv": c.id, "roster": c.roster.len() }));
                }
                return match r {
                    Ok(c) => reply(json!({ "conversation": c }).to_string()),
                    Err(e) => reply(json!({ "error": format!("{e:#}") }).to_string()),
                };
            }
            Some(Op::ConvInvite { conv, members }) => {
                let me = from.to_string();
                let known = self.member_ids();
                let r = self.convs.invite(&me, &conv, &members, &|m: &str| known.contains(m));
                if let Ok(c) = &r {
                    self.audit.record("conv_invite", Some(&me), json!({ "conv": c.id, "roster": c.roster.len(), "roster_version": c.roster_version }));
                }
                return match r {
                    Ok(c) => reply(json!({ "conversation": c }).to_string()),
                    Err(e) => reply(json!({ "error": format!("{e:#}") }).to_string()),
                };
            }
            Some(Op::ConvPost { conv, text, request_id, sig }) => {
                let me = from.to_string();
                let r = self.convs.post(&me, from.as_bytes(), &conv, &text, &request_id, &sig);
                if let Ok(e) = &r {
                    self.audit.record("conv_post", Some(&me), json!({ "conv": conv, "seq": e.seq, "bytes": text.len(), "text_hash": blake3::hash(text.as_bytes()).to_hex().to_string() }));
                }
                return match r {
                    Ok(e) => reply(json!({ "entry": e }).to_string()),
                    Err(e) => reply(json!({ "error": format!("{e:#}") }).to_string()),
                };
            }
            Some(Op::ConvRead { conv, since, mark }) => {
                return match self.convs.read(&from.to_string(), &conv, since, mark) {
                    Ok(v) => reply(json!({ "conversation": v }).to_string()),
                    Err(e) => reply(json!({ "error": format!("{e:#}") }).to_string()),
                };
            }
            Some(Op::CapList) => return reply(json!({ "capabilities": self.caps.book.describe() }).to_string()),
            Some(Op::Cap { name, args, call_id }) => {
                return match self.caps.submit(&from.to_string(), &name, args, &call_id) {
                    Ok(c) => reply(json!({ "call": c.signed(&self.attest_key) }).to_string()),
                    Err(e) => reply(json!({ "error": format!("{e:#}") }).to_string()),
                }
            }
            Some(Op::CapPoll { call_id }) => {
                return match self.caps.book.view(&from.to_string(), &call_id) {
                    Some(c) => reply(json!({ "call": c.signed(&self.attest_key) }).to_string()),
                    None => reply(json!({ "error": "no such call" }).to_string()),
                }
            }
            Some(Op::Feed { since, wait_secs }) => {
                let me = from.to_string();
                self.audit.chat.touch(&me);
                let mut ids: Vec<String> = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).members.keys().map(std::string::ToString::to_string).collect();
                ids.sort();
                let wait = Duration::from_secs(wait_secs);
                return reply(self.audit.chat.wait_since(&me, since, 200, &ids, wait).await.to_json().to_string());
            }
            // Handled in `route`, which has the grant it needs to authorize the specifics.
            Some(Op::Steer { .. } | Op::Handoff { .. }) => return reply("steering is handled by the router".into()),
            Some(Op::Latent) => {
                let t0 = std::time::Instant::now();
                let r = self.latent_deposit(from, msg);
                self.audit_op(from, "latent", &r, t0, json!({}));
                finish(r)
            }
            Some(Op::LatentPull { hub, since }) => self.latent_pull(&hub, since),
            Some(Op::LatentReveal { seq }) => {
                let t0 = std::time::Instant::now();
                let r = self.latent_reveal(from, seq, msg);
                self.audit_op(from, "latent_reveal", &r, t0, json!({ "seq": seq }));
                finish(r)
            }
            Some(Op::Browse { url, mode, wait_ms }) => {
                let t0 = std::time::Instant::now();
                let r = self.browse(from, &url, mode, wait_ms).await;
                self.audit.record(
                    "browse",
                    Some(&from.to_string()),
                    json!({
                        "url": browse::clip_url(&url),
                        "mode": mode,
                        "ok": r.is_ok(),
                        "ms": t0.elapsed().as_millis() as u64,
                        "reason": r.as_ref().err().map(|e| audit::clip(&format!("{e:#}"))),
                        "blocked": r.as_ref().ok().and_then(|t| wire::meta(t, "blocked")).and_then(Value::as_array).map(Vec::len),
                    }),
                );
                finish(r)
            }
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
                    "secrets": spec.secrets,
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
    pub(crate) fn audit_op(
        &self,
        from: EndpointId,
        event: &str,
        result: &Result<Task>,
        started: std::time::Instant,
        mut details: Value,
    ) {
        let Some(obj) = details.as_object_mut() else {
            return;
        };
        obj.insert(
            "ms".into(),
            json!(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        );
        match result {
            Ok(t) => {
                obj.insert("ok".into(), json!(true));
                obj.insert("state".into(), json!(format!("{:?}", t.status.state)));
                for (key, out) in [
                    ("exitCode", "exit_code"),
                    ("timedOut", "timed_out"),
                    ("runner", "runner"),
                    ("commit", "commit"),
                    ("reference", "reference"),
                ] {
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
        self.cfg
            .repo
            .clone()
            .ok_or_else(|| refuse("this room has no git repository"))
    }

    async fn browse(
        &self,
        from: EndpointId,
        url: &str,
        mode: browse::Mode,
        wait_ms: Option<u64>,
    ) -> Result<Task> {
        let b = self
            .cfg
            .browse
            .clone()
            .ok_or_else(|| refuse("this room has no browser"))?;
        let r = b
            .browse(&from.to_string(), url, mode, wait_ms)
            .await
            .map_err(|e| match e {
                browse::BrowseError::Refused(m) => refuse(m),
                browse::BrowseError::Failed(e) => e,
            })?;
        let mut artifacts = Vec::new();
        if let Some(t) = &r.text {
            artifacts.push(wire::text_artifact("text", t.clone()));
        }
        if let Some(h) = &r.html {
            artifacts.push(wire::text_artifact("html", h.clone()));
        }
        if let Some(png) = &r.screenshot {
            let (name, media) = if png.starts_with(b"\x89PNG") {
                ("screenshot.png", "image/png")
            } else {
                ("screenshot.jpg", "image/jpeg")
            };
            artifacts.push(wire::raw_artifact("screenshot", name, media, png.clone()));
        }
        Ok(wire::task(
            TaskState::Completed,
            None,
            artifacts,
            vec![
                ("url", json!(r.url)),
                ("title", json!(r.title)),
                ("status", json!(r.status)),
                ("blocked", json!(r.blocked)),
                ("requests", json!(r.requests)),
                ("timed_out", json!(r.timed_out)),
                ("truncated", json!(r.truncated)),
            ],
        ))
    }

    async fn pull(&self, refname: &str) -> Result<Task> {
        let repo = self.repo()?;
        playbox_git::check_name(refname).map_err(|e| refuse(format!("{e:#}")))?;
        let head = playbox_git::rev_parse(&repo, refname).await?;
        let bundle = playbox_git::bundle_create(&repo, refname).await?;
        Ok(wire::task(
            TaskState::Completed,
            None,
            vec![wire::raw_artifact(
                "bundle",
                "ref.bundle",
                wire::MEDIA_BUNDLE,
                bundle,
            )],
            vec![("head", json!(head))],
        ))
    }

    async fn push(&self, from: EndpointId, branch: &str, bundle: Option<&[u8]>) -> Result<Task> {
        let repo = self.repo()?;
        let branch = playbox_git::check_name(branch).map_err(|e| refuse(format!("{e:#}")))?;
        let bundle = bundle
            .ok_or_else(|| refuse("push needs the git bundle as a raw part of the message"))?;
        {
            let mut pushed = self
                .pushed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let head =
            playbox_git::bundle_import(&repo, bundle, &format!("refs/heads/{branch}"), &reference)
                .await?;
        Ok(wire::task(
            TaskState::Completed,
            None,
            vec![],
            vec![("reference", json!(reference)), ("head", json!(head))],
        ))
    }

    /// Run a member's task under the abuse limits, and stop it if the member is revoked while it runs.
    /// Stopping drops the run, which kills its sandbox.
    async fn run(&self, from: EndpointId, base: &str, spec: TaskSpec) -> Result<Task> {
        let _admission = self
            .limiter
            .admit(from)
            .map_err(|e| refuse(e.to_string()))?;
        tokio::select! {
            r = self.run_inner(from, base, spec) => r,
            () = self.until_revoked(from) => {
                self.audit.record("task_cancelled", Some(&from.to_string()), json!({ "reason": "member revoked" }));
                Err(refuse("cancelled: your access to this room was revoked"))
            }
        }
    }

    /// Resolves once `who` has been revoked (here or by a notice that reached this host).
    /// Ids of the room's current members, as strings.
    fn member_ids(&self) -> std::collections::HashSet<String> {
        self.state
            .lock()
            .expect("state lock")
            .members
            .keys()
            .map(std::string::ToString::to_string)
            .collect()
    }

    async fn until_revoked(&self, who: EndpointId) {
        loop {
            if self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .banned
                .contains(&who)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn run_inner(&self, from: EndpointId, base: &str, spec: TaskSpec) -> Result<Task> {
        let runner = self
            .cfg
            .runner
            .clone()
            .ok_or_else(|| refuse("this room runs no tasks"))?;
        let (output, patch, extra) = if let Some(ws) = &self.cfg.workspace {
            let _permit = self.tasks.acquire().await?;
            let short: String = from.to_string().chars().take(16).collect();
            let branch = format!("members/{short}");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
                .unwrap_or(0);
            let mut repo = ws.lock().await;
            if repo.head(&branch).is_err() {
                repo.fork(&branch, base)?;
            }
            let mut wsp = repo.workspace(&branch)?;
            let run = playbox_runner::workspace::run_in_workspace(
                &mut repo,
                &mut wsp,
                runner.as_ref(),
                &spec,
                &from.to_string(),
                &format!("rpc-{now}"),
                now,
                self.cfg.scratch_quota,
            )
            .await?;
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
            let mut guard = WorktreeGuard {
                repo: repo.clone(),
                wt: Some(playbox_git::worktree_add(&repo, &commit).await?),
            };
            let result = async {
                let wt = guard.wt.as_ref().expect("worktree present");
                let output = playbox_runner::run_with_quota(
                    runner.as_ref(),
                    &spec,
                    wt.path(),
                    self.cfg.scratch_quota,
                )
                .await?;
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
        let mut artifacts = vec![
            wire::text_artifact("stdout", output.stdout),
            wire::text_artifact("stderr", output.stderr),
        ];
        if !patch.is_empty() {
            artifacts.push(wire::raw_artifact(
                "patch",
                "changes.patch",
                wire::MEDIA_PATCH,
                patch,
            ));
        }
        let mut meta = vec![
            ("runner", json!(runner.name())),
            ("timedOut", json!(output.timed_out)),
        ];
        if let Some(d) = &output.policy_digest {
            meta.push(("policyDigest", json!(d)));
        }
        if let Some(c) = output.exit_code {
            meta.push(("exitCode", json!(c)));
        }
        if !output.secrets_used.is_empty() {
            meta.push(("secretsUsed", json!(output.secrets_used)));
        }
        meta.extend(extra);
        Ok(wire::task(
            if ok {
                TaskState::Completed
            } else {
                TaskState::Failed
            },
            text,
            artifacts,
            meta,
        ))
    }

    /// Answer one request. The flag says whether the caller proved itself (a valid invite or grant),
    /// which is what lets its connection send large requests afterwards.
    async fn dispatch(&self, from: EndpointId, bytes: &[u8]) -> (JsonRpcResponse, bool) {
        let frame: wire::Frame = match serde_json::from_slice(bytes) {
            Ok(f) => f,
            Err(e) => {
                return (
                    JsonRpcResponse::error(
                        JsonRpcId::Null,
                        A2AError::parse_error(e.to_string()).to_jsonrpc_error(),
                    ),
                    false,
                )
            }
        };
        let id = frame.rpc.id.clone();
        let public = wire::is_public(&frame.rpc.method);
        match self.route(from, &frame).await {
            Ok(v) => (JsonRpcResponse::success(id, v), !public),
            Err(e) => {
                if !public && refusal_is_new(&from.to_string(), &frame.rpc.method, &e.message) {
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
    fn take(
        map: &Arc<Mutex<HashMap<EndpointId, usize>>>,
        id: EndpointId,
        max: usize,
    ) -> Option<PeerSlot> {
        let mut m = map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = m.entry(id).or_default();
        if *n >= max.max(1) {
            return None;
        }
        *n += 1;
        Some(PeerSlot {
            map: map.clone(),
            id,
        })
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut m = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            self.audit.record(
                "conn_refused",
                Some(&who),
                json!({ "why": "host is at its connection limit" }),
            );
            conn.close(2u32.into(), b"too many connections");
            return Ok(());
        };
        let Some(_peer_slot) =
            PeerSlot::take(&self.peer_conns, from, self.cfg.max_connections_per_peer)
        else {
            self.audit.record(
                "conn_refused",
                Some(&who),
                json!({ "why": "too many connections from this identity" }),
            );
            conn.close(2u32.into(), b"too many connections from this identity");
            return Ok(());
        };
        let mut authenticated = false;
        let mut unproven = 0u32;
        loop {
            if !authenticated {
                unproven += 1;
                if unproven > MAX_UNAUTHENTICATED_REQUESTS {
                    conn.close(3u32.into(), b"too many requests without a grant");
                    return Ok(());
                }
            }
            let (patience, cap) = if authenticated {
                (self.cfg.authenticated_patience, MAX_FRAME)
            } else {
                (self.cfg.idle_timeout, MAX_UNAUTHENTICATED_REQUEST)
            };
            let Ok(Ok((mut send, mut recv))) =
                tokio::time::timeout(patience, conn.accept_bi()).await
            else {
                break;
            };
            let Ok(Ok(bytes)) = tokio::time::timeout(patience, recv.read_to_end(cap)).await else {
                self.audit.record("request_dropped", Some(&who), json!({ "why": "too large or too slow", "limit_bytes": cap, "authenticated": authenticated }));
                conn.close(1u32.into(), b"request too large or too slow");
                return Ok(());
            };
            let (resp, proved) = self.dispatch(from, &bytes).await;
            authenticated |= proved;
            let Ok(out) = serde_json::to_vec(&resp) else {
                break;
            };
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

/// A member that keeps being refused for the same reason (an expired grant, polled every few seconds) is recorded once a minute, not once
/// per call: the log and the feed stay readable, and the room's history is not pushed out by it.
fn refusal_is_new(from: &str, method: &str, reason: &str) -> bool {
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    type Seen = std::collections::HashMap<(String, String, String), Instant>;
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    let Ok(mut seen) = SEEN.get_or_init(Default::default).lock() else {
        return true;
    };
    let now = Instant::now();
    if seen.len() > 4096 {
        seen.retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
    }
    let key = (
        from.to_string(),
        method.to_string(),
        reason.chars().take(80).collect(),
    );
    match seen.get(&key) {
        Some(t) if now.duration_since(*t) < Duration::from_secs(60) => false,
        _ => {
            seen.insert(key, now);
            true
        }
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::refusal_is_new;

    #[test]
    fn the_same_refusal_is_recorded_once_a_minute() {
        assert!(refusal_is_new(
            "m1",
            "feed",
            "authorization failed: expired"
        ));
        assert!(!refusal_is_new(
            "m1",
            "feed",
            "authorization failed: expired"
        ));
        assert!(
            refusal_is_new("m2", "feed", "authorization failed: expired"),
            "another member is its own case"
        );
        assert!(
            refusal_is_new("m1", "say", "authorization failed: expired"),
            "so is another method"
        );
        assert!(refusal_is_new("m1", "feed", "a different reason"));
    }
}
