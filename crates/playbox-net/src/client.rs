//! A member's connection to a room host.

use crate::bind::{bind, bind_chosen, Bind};
use crate::grants;
use crate::keys::random_bytes;
use crate::link::Link;
use crate::wire::{self, Op};
use crate::{
    browse, capabilities, chat, conversations, shared_memory, Transport, TransportChoice, ALPN,
    CONNECT_ATTEMPTS, CONNECT_ATTEMPT_TIMEOUT, MAX_FRAME, MAX_UNAUTHENTICATED_REQUEST,
};
use a2a::{
    GetTaskRequest, JsonRpcRequest, JsonRpcResponse, ListTasksResponse, Part, SendMessageRequest,
    SendMessageResponse, Task, TaskState,
};
use anyhow::{anyhow, bail, Context, Result};
use iroh::{endpoint::Connection, Endpoint, EndpointAddr, EndpointId, SecretKey};
use playbox_core::attest::{self, PlatformVerifier, Tier, Verified};
use playbox_runner::{TaskOutput, TaskSpec};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicI64, Ordering},
        Mutex,
    },
    time::Duration,
};

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

/// Why a `say` did not complete: the host said no (final), or no answer came (the message may have been posted).
enum SayError {
    Refused(anyhow::Error),
    Transport(anyhow::Error),
}

/// Lower-case hex of `b`.
fn hex_id(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub struct Client {
    pub(crate) endpoint: Endpoint,
    /// Where the host was reached, so a closed connection can be reopened.
    addr: EndpointAddr,
    conn: Connection,
    grant: Mutex<Option<String>>,
    next_id: AtomicI64,
    transport: Transport,
    /// Whether this connection has had a request accepted with an invite or grant, so the host will
    /// take a large request on it.
    warm: std::sync::atomic::AtomicBool,
}

impl Client {
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn connect(
        addr: EndpointAddr,
        secret: SecretKey,
        transport: impl Into<Transport>,
    ) -> Result<Self> {
        let transport = transport.into();
        // A host on this machine (its link lists a loopback address) is dialled over loopback first, so this process never listens on the
        // network. Reaching a host on another machine, whose loopback address means nothing here, fails fast and falls through.
        if transport == Transport::Local && !bind_chosen() {
            let local: Vec<_> = addr
                .ip_addrs()
                .filter(|a| a.ip().is_loopback())
                .map(|a| iroh::TransportAddr::Ip(*a))
                .collect();
            if !local.is_empty() {
                let near = EndpointAddr::from_parts(addr.id, local);
                if let Ok(c) = Self::dial(
                    near,
                    addr.clone(),
                    secret.clone(),
                    &transport,
                    Bind::GuestLoopback,
                    1,
                    Duration::from_secs(3),
                )
                .await
                {
                    return Ok(c);
                }
            }
        }
        Self::dial(
            addr.clone(),
            addr,
            secret,
            &transport,
            Bind::Guest,
            CONNECT_ATTEMPTS,
            CONNECT_ATTEMPT_TIMEOUT,
        )
        .await
    }

    /// Connect to `target`, remembering `addr` (the host's full address) so a closed connection can be reopened the usual way.
    async fn dial(
        target: EndpointAddr,
        addr: EndpointAddr,
        secret: SecretKey,
        transport: &Transport,
        how: Bind,
        attempts: u32,
        timeout: Duration,
    ) -> Result<Self> {
        let (endpoint, _) = bind(secret, transport, how).await?;
        // A handshake that never completes is retried on the same endpoint: in practice a stuck
        // first attempt is far more common than a host that is really unreachable.
        let mut last = anyhow!("no connection attempt was made");
        let mut conn = None;
        for _ in 0..attempts {
            match tokio::time::timeout(timeout, endpoint.connect(target.clone(), ALPN)).await {
                Ok(Ok(c)) => {
                    conn = Some(c);
                    break;
                }
                Ok(Err(e)) => last = e.into(),
                Err(_) => last = anyhow!("timed out after {}s", timeout.as_secs()),
            }
        }
        let conn = conn
            .ok_or_else(|| last.context(format!("could not connect after {attempts} attempts")))?;
        Ok(Self {
            endpoint,
            addr,
            conn,
            grant: Mutex::new(None),
            next_id: AtomicI64::new(1),
            transport: transport.clone(),
            warm: Default::default(),
        })
    }

    /// The addresses this client's own sockets are bound to: empty when it dials out only through a relay, loopback only for a host on this machine.
    pub fn bound_sockets(&self) -> Vec<std::net::SocketAddr> {
        self.endpoint.bound_sockets()
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// Whether the connection to the host has been closed. A host closes an authenticated connection that sat
    /// idle for 15 minutes, so a long-lived member will see this and should [`Client::reconnect`].
    pub fn is_closed(&self) -> bool {
        self.conn.close_reason().is_some()
    }

    /// A new connection to the same host as the same member, with the same grant. The host key is the one this
    /// client already reached, so the connection is authenticated to the same host; whether that host is still
    /// running the software a caller vetted is for the caller to check again.
    ///
    /// # Errors
    ///
    /// Returns an error if the host cannot be reached.
    pub async fn reconnect(&self) -> Result<Client> {
        self.sibling(self.addr.clone()).await
    }

    /// The id of the host this client is connected to.
    pub fn host_id(&self) -> EndpointId {
        self.conn.remote_id()
    }

    /// Open a connection to another host of the same room (a replica), as the same member: the same
    /// identity key and the same grant, which any host that knows the room's public key accepts.
    /// The caller should vet the new host (`verify_host`) before sending it anything.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn sibling(&self, addr: EndpointAddr) -> Result<Client> {
        let c = Client::connect(
            addr,
            self.endpoint.secret_key().clone(),
            self.transport.clone(),
        )
        .await?;
        if let Some(g) = self.grant() {
            c.set_grant(g);
        }
        Ok(c)
    }

    /// The grant this client presents, if it has joined.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned, which only happens after another thread panicked while holding it.
    pub fn grant(&self) -> Option<String> {
        self.grant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// # Panics
    ///
    /// Panics if the internal lock is poisoned, which only happens after another thread panicked while holding it.
    pub fn set_grant(&self, grant: String) {
        *self
            .grant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(grant);
    }

    /// Narrow this client's grant with extra Biscuit checks. Works offline and cannot be undone.
    /// Example: `check if operation("pull");`
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn attenuate(&self, checks: &str) -> Result<()> {
        let g = self.grant().ok_or_else(|| anyhow!("not joined"))?;
        self.set_grant(grants::attenuate(&g, checks)?);
        Ok(())
    }

    /// One JSON-RPC call with this client's grant, returning the whole response (including any error code).
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<JsonRpcResponse> {
        let body = self.frame_bytes(method, params)?;
        // The host reads only a small first request from a connection that has not proved itself. If
        // this one is large and nothing has been accepted on the connection yet, say something small first.
        if body.len() > MAX_UNAUTHENTICATED_REQUEST
            && !self.warm.load(Ordering::Relaxed)
            && !wire::is_public(method)
        {
            let ping = Op::Echo {
                text: "warm-up".into(),
            }
            .message(vec![]);
            let req = SendMessageRequest {
                message: ping,
                configuration: None,
                metadata: None,
                tenant: None,
            };
            let small = self.frame_bytes(a2a::methods::SEND_MESSAGE, serde_json::to_value(req)?)?;
            let resp = self
                .exchange(&small, a2a::methods::SEND_MESSAGE)
                .await
                .context("proving this connection before a large request")?;
            if let Some(e) = resp.error {
                bail!(
                    "could not prove this connection before a large request: {}",
                    e.message
                );
            }
        }
        self.exchange(&body, method).await
    }

    fn frame_bytes(&self, method: &str, params: Value) -> Result<Vec<u8>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = wire::Frame::new(
            JsonRpcRequest::new(id.into(), method, Some(params)),
            self.grant().as_deref(),
        );
        Ok(serde_json::to_vec(&frame)?)
    }

    /// Send one already-built request and read the response.
    async fn exchange(&self, body: &[u8], method: &str) -> Result<JsonRpcResponse> {
        let (mut send, mut recv) = self.conn.open_bi().await?;
        send.write_all(body).await?;
        send.finish()?;
        let resp: JsonRpcResponse =
            serde_json::from_slice(&recv.read_to_end(MAX_FRAME).await?).context("bad response")?;
        if resp.error.is_none() && !wire::is_public(method) {
            self.warm.store(true, Ordering::Relaxed);
        }
        Ok(resp)
    }

    /// One JSON-RPC call with this client's grant; returns the `result`, or the error's message.
    /// The typed helpers below use it; call it directly for A2A methods that have no helper.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
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
        let req = SendMessageRequest {
            message: op.message(extra),
            configuration: None,
            metadata: None,
            tenant: None,
        };
        Ok(serde_json::from_value(
            self.call(a2a::methods::SEND_MESSAGE, serde_json::to_value(req)?)
                .await?,
        )?)
    }

    /// A task result that must have completed; otherwise the host's reason.
    pub(crate) async fn send_task(&self, what: &str, op: &Op, extra: Vec<Part>) -> Result<Task> {
        match self
            .send(op, extra)
            .await
            .map_err(|e| anyhow!("{what} refused: {e:#}"))?
        {
            SendMessageResponse::Task(t) if t.status.state == TaskState::Completed => Ok(t),
            SendMessageResponse::Task(t) => bail!("{what} refused: {}", wire::status_text(&t)),
            SendMessageResponse::Message(m) => {
                bail!("unexpected message instead of a task: {:?}", m.text())
            }
        }
    }

    /// Present the invite; returns this client's member id.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn join(&self, invite: &str) -> Result<String> {
        let r = self
            .call(wire::METHOD_JOIN, json!({ "invite": invite, "protocol": wire::PROTOCOL, "version": env!("CARGO_PKG_VERSION") }))
            .await
            .map_err(|e| anyhow!("join refused: {e:#}"))?;
        wire::check_protocol(
            r.get("protocol").and_then(Value::as_u64),
            r.get("version").and_then(Value::as_str).unwrap_or("?"),
        )
        .map_err(|e| anyhow!("{e}"))?;
        let grant = r
            .get("grant")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("join response has no grant"))?;
        self.set_grant(grant.to_string());
        Ok(r.get("member")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn echo(&self, text: &str) -> Result<String> {
        match self
            .send(
                &Op::Echo {
                    text: text.to_string(),
                },
                vec![],
            )
            .await
            .map_err(|e| anyhow!("echo refused: {e:#}"))?
        {
            SendMessageResponse::Message(m) => Ok(m.text().unwrap_or_default().to_string()),
            SendMessageResponse::Task(t) => bail!("echo refused: {}", wire::status_text(&t)),
        }
    }

    /// Say something to the room. `name` is a label for display; the member id is the identity.
    ///
    /// The message carries a fresh request id, and if the connection fails before an answer arrives the same message is sent
    /// again, up to three times, on a new connection to the same host. The host posts a request id once, so a lost reply
    /// never says it twice. A refusal from the host (rate limit, empty text) is final and is not retried.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn say(&self, text: &str, name: Option<&str>) -> Result<u64> {
        let id = hex_id(&random_bytes()?[..12]);
        let mut last = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(200 << attempt)).await;
            }
            let outcome = if attempt == 0 {
                self.say_with_id(&id, text, name).await
            } else {
                match self.sibling(self.addr.clone()).await {
                    Ok(c) => c.say_with_id(&id, text, name).await,
                    Err(e) => Err(SayError::Transport(e)),
                }
            };
            match outcome {
                Ok(seq) => return Ok(seq),
                Err(SayError::Refused(e)) => return Err(e),
                Err(SayError::Transport(e)) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("say failed")))
            .context("the room did not answer; the message may or may not have been posted")
    }

    /// [`Client::say`] with an id the caller chooses (1 to 64 letters, digits, - _ .), sent once on this connection. A caller
    /// that reconnects itself resends the same id and the host posts the message once.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn say_with_id_once(
        &self,
        request_id: &str,
        text: &str,
        name: Option<&str>,
    ) -> Result<u64> {
        self.say_with_id(request_id, text, name)
            .await
            .map_err(|e| match e {
                SayError::Refused(e) | SayError::Transport(e) => e,
            })
    }

    async fn say_with_id(
        &self,
        request_id: &str,
        text: &str,
        name: Option<&str>,
    ) -> std::result::Result<u64, SayError> {
        let op = Op::Say {
            text: text.to_string(),
            name: name.map(str::to_string),
            request_id: Some(request_id.to_string()),
        };
        let req = SendMessageRequest {
            message: op.message(vec![]),
            configuration: None,
            metadata: None,
            tenant: None,
        };
        let params = serde_json::to_value(req).map_err(|e| SayError::Refused(e.into()))?;
        // Failing to get any answer is a transport problem and may be retried; an answer that says no is final.
        let resp = self
            .call_raw(a2a::methods::SEND_MESSAGE, params)
            .await
            .map_err(SayError::Transport)?;
        if let Some(e) = resp.error {
            return Err(SayError::Refused(anyhow!("say refused: {}", e.message)));
        }
        let result = resp
            .result
            .ok_or_else(|| SayError::Transport(anyhow!("empty response")))?;
        let reply: SendMessageResponse =
            serde_json::from_value(result).map_err(|e| SayError::Refused(e.into()))?;
        let v: Value = match reply {
            SendMessageResponse::Message(m) => serde_json::from_str(m.text().unwrap_or("{}"))
                .map_err(|e| SayError::Refused(e.into()))?,
            SendMessageResponse::Task(t) => {
                return Err(SayError::Refused(anyhow!(
                    "say refused: {}",
                    wire::status_text(&t)
                )))
            }
        };
        if let Some(e) = v.get("error").and_then(Value::as_str) {
            return Err(SayError::Refused(anyhow!("{e}")));
        }
        Ok(v.get("seq").and_then(Value::as_u64).unwrap_or(0))
    }

    /// What was said and what happened after `since`, and who is present.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn feed(&self, since: u64) -> Result<chat::Feed> {
        let v = self
            .reply_json(
                &Op::Feed {
                    since,
                    wait_secs: 0,
                },
                "feed",
            )
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// [`Client::feed`], but when nothing is new the host holds the request open for up to `wait_secs` (it caps this) and answers
    /// the moment something arrives. A host that predates this answers at once, so callers should still pace their loop.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn feed_wait(&self, since: u64, wait_secs: u64) -> Result<chat::Feed> {
        let v = self
            .reply_json(&Op::Feed { since, wait_secs }, "feed")
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Add an entry to the room's shared memory. The same text (or `key`) from you replaces the entry, so a retry is safe.
    ///
    /// # Errors
    ///
    /// Returns an error if the room has no shared memory, the entry is refused, or the host cannot be reached.
    pub async fn remember(
        &self,
        text: &str,
        key: Option<&str>,
    ) -> Result<shared_memory::Remembered> {
        let op = Op::Remember {
            text: text.to_string(),
            key: key.map(str::to_string),
        };
        let v = self.reply_json(&op, "remember").await?;
        if let Some(e) = v.get("error").and_then(Value::as_str) {
            bail!("remember refused: {e}");
        }
        Ok(serde_json::from_value(v)?)
    }

    /// Ask the room's shared memory. Only entries the host accepted, by members who are still in the room.
    ///
    /// # Errors
    ///
    /// Returns an error if the room has no shared memory, the query is refused, or the host cannot be reached.
    pub async fn recall(
        &self,
        query: &str,
        max_tokens: Option<u32>,
        author: Option<&str>,
    ) -> Result<Vec<shared_memory::Hit>> {
        let op = Op::Recall {
            query: query.to_string(),
            max_tokens,
            author: author.map(str::to_string),
        };
        let v = self.reply_json(&op, "recall").await?;
        if let Some(e) = v.get("error").and_then(Value::as_str) {
            bail!("recall refused: {e}");
        }
        Ok(
            serde_json::from_value(v.get("hits").cloned().unwrap_or(Value::Null))
                .unwrap_or_default(),
        )
    }

    /// Start a conversation with `members` (their ids, as `members()` lists them).
    ///
    /// # Errors
    ///
    /// Returns an error if the host refuses: an unknown member, a bad title, too few or too many people.
    pub async fn conv_new(&self, title: &str, members: &[String]) -> Result<Value> {
        let v = self
            .reply_json(
                &Op::ConvNew {
                    title: title.to_string(),
                    members: members.to_vec(),
                },
                "conversation",
            )
            .await?;
        conv_field(&v, "conversation")
    }

    /// Add members to a conversation you created.
    ///
    /// # Errors
    ///
    /// Returns an error if the host refuses.
    pub async fn conv_invite(&self, conv: &str, members: &[String]) -> Result<Value> {
        let v = self
            .reply_json(
                &Op::ConvInvite {
                    conv: conv.to_string(),
                    members: members.to_vec(),
                },
                "conversation",
            )
            .await?;
        conv_field(&v, "conversation")
    }

    /// The conversations you are on, with unread counts.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn conversations(&self) -> Result<Value> {
        let v = self.reply_json(&Op::ConvList, "conversations").await?;
        Ok(v.get("conversations").cloned().unwrap_or(Value::Null))
    }

    /// Read a conversation after `since`; `mark` moves your read mark. What others wrote is untrusted text.
    ///
    /// # Errors
    ///
    /// Returns an error if you are not on the roster.
    pub async fn conv_read(&self, conv: &str, since: u64, mark: bool) -> Result<Value> {
        let v = self
            .reply_json(
                &Op::ConvRead {
                    conv: conv.to_string(),
                    since,
                    mark,
                },
                "conversation",
            )
            .await?;
        conv_field(&v, "conversation")
    }

    /// Post to a conversation, signed with your key. Retried on a new connection if no answer arrives; the host stores a post once
    /// per request id, so a retry cannot post twice. Returns the stored entry (with its sequence number).
    ///
    /// # Errors
    ///
    /// Returns an error if the host refuses or never answers.
    pub async fn conv_say(&self, conv: &str, text: &str) -> Result<Value> {
        let rid = hex_id(&random_bytes()?[..12]);
        let sig = hex_id(
            &self
                .endpoint
                .secret_key()
                .sign(&conversations::signed_text(conv, &rid, text))
                .to_bytes(),
        );
        let op = Op::ConvPost {
            conv: conv.to_string(),
            text: text.to_string(),
            request_id: rid,
            sig,
        };
        let mut last = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(200 << attempt)).await;
            }
            let sent = if attempt == 0 {
                self.reply_json(&op, "post").await
            } else {
                match self.sibling(self.addr.clone()).await {
                    Ok(c) => c.reply_json(&op, "post").await,
                    Err(e) => Err(e),
                }
            };
            match sent {
                Ok(v) => return conv_field(&v, "entry"),
                // The host answered no: final. Anything else may be the connection.
                Err(e) if format!("{e:#}").contains("refused") => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("post failed")))
            .context("the room did not answer; the post may or may not have been stored")
    }

    /// The capabilities the host offers: names, parameters and whether each call needs the owner's approval.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn capabilities(&self) -> Result<Value> {
        let v = self.reply_json(&Op::CapList, "capabilities").await?;
        Ok(v.get("capabilities").cloned().unwrap_or(Value::Null))
    }

    /// Start a capability call. `call_id` makes it run once however many times it is sent; the answer is the call as the host
    /// records it (`awaiting_approval`, `running`, or already `done`).
    ///
    /// # Errors
    ///
    /// Returns an error if the host refuses (no grant for the capability, arguments outside its bounds, an id reused for a
    /// different request).
    pub async fn call_capability(
        &self,
        name: &str,
        args: std::collections::BTreeMap<String, String>,
        call_id: &str,
    ) -> Result<capabilities::Call> {
        let op = Op::Cap {
            name: name.to_string(),
            args,
            call_id: call_id.to_string(),
        };
        let v = self.reply_json(&op, "capability call").await?;
        call_of(&v, self.host_id())
    }

    /// The state of your own call.
    ///
    /// # Errors
    ///
    /// Returns an error if there is no such call.
    pub async fn poll_capability(&self, call_id: &str) -> Result<capabilities::Call> {
        let v = self
            .reply_json(
                &Op::CapPoll {
                    call_id: call_id.to_string(),
                },
                "capability poll",
            )
            .await?;
        call_of(&v, self.host_id())
    }

    /// Call a capability and wait up to `wait` for it to finish. A connection that drops mid-wait is not a failure of the call: the
    /// same id is polled again on a new connection.
    ///
    /// # Errors
    ///
    /// As [`Client::call_capability`]; also when `wait` passes with the call still open (the call is not cancelled, poll it later).
    pub async fn call_capability_wait(
        &self,
        name: &str,
        args: std::collections::BTreeMap<String, String>,
        call_id: &str,
        wait: Duration,
    ) -> Result<capabilities::Call> {
        let mut call = self.call_capability(name, args, call_id).await?;
        let until = std::time::Instant::now() + wait;
        while matches!(
            call.state,
            capabilities::State::Running | capabilities::State::AwaitingApproval
        ) {
            if std::time::Instant::now() > until {
                bail!(
                    "call {call_id} is still {:?}; poll it again later",
                    call.state
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            call = self.poll_capability(call_id).await?;
        }
        Ok(call)
    }

    async fn reply_json(&self, op: &Op, what: &str) -> Result<Value> {
        match self
            .send(op, vec![])
            .await
            .map_err(|e| anyhow!("{what} refused: {e:#}"))?
        {
            SendMessageResponse::Message(m) => Ok(serde_json::from_str(m.text().unwrap_or("{}"))?),
            SendMessageResponse::Task(t) => bail!("{what} refused: {}", wire::status_text(&t)),
        }
    }

    /// The host's public agent card.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn agent_card(&self) -> Result<a2a::AgentCard> {
        Ok(serde_json::from_value(
            self.call(wire::METHOD_AGENT_CARD, Value::Null).await?,
        )?)
    }

    /// Ask the host to attest to what it runs, and check the answer.
    ///
    /// The statement must be signed by the key this connection already authenticated, must echo a
    /// fresh nonce, and the tier it *achieves* (see [`playbox_core::attest`]) must reach `min`.
    /// `verifiers` check platform evidence; with none, the achieved tier is T0.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn verify_host(
        &self,
        min: Tier,
        verifiers: &[&dyn PlatformVerifier],
    ) -> Result<Verified> {
        let nonce: String = random_bytes()?[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let r = self
            .call(wire::METHOD_ATTESTATION, json!({ "nonce": nonce }))
            .await?;
        let text = r
            .get("statement")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("no statement in response"))?;
        let mut v = attest::verify(text, verifiers)
            .map_err(|e| anyhow!("host attestation invalid: {e}"))?;
        if v.statement.subject != *self.conn.remote_id().as_bytes() {
            bail!("host attestation is for a different key than the one this connection authenticated");
        }
        if v.statement.nonce != nonce {
            bail!("host attestation does not answer our nonce (replayed?)");
        }
        // Only now, with the host's identity and freshness settled, put its hardware to the test.
        if let Some(ch) = v.challenge.clone() {
            let req: String = ch.payload.iter().map(|b| format!("{b:02x}")).collect();
            match self
                .call(wire::METHOD_ACTIVATE, json!({ "request": req }))
                .await
            {
                Ok(r) => {
                    let h = r
                        .get("response")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let bytes: Vec<u8> = (0..h.len() / 2)
                        .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap_or(0))
                        .collect();
                    v.complete_challenge(&bytes);
                }
                Err(e) => v.notes.push(format!(
                    "host could not answer the hardware challenge: {e:#}"
                )),
            }
        }
        if v.achieved < min {
            bail!(
                "host achieves {} but {} was required ({})",
                v.achieved,
                min,
                v.notes.join("; ")
            );
        }
        Ok(v)
    }

    /// A task this member created earlier (byte payloads are not kept; see [`wire::without_bytes`]).
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn get_task(&self, id: &str) -> Result<Task> {
        Ok(serde_json::from_value(
            self.call(
                a2a::methods::GET_TASK,
                serde_json::to_value(GetTaskRequest {
                    id: id.into(),
                    history_length: None,
                    tenant: None,
                })?,
            )
            .await?,
        )?)
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn list_tasks(&self) -> Result<Vec<Task>> {
        let r: ListTasksResponse =
            serde_json::from_value(self.call(a2a::methods::LIST_TASKS, json!({})).await?)?;
        Ok(r.tasks)
    }

    /// Read a page through the room host's browser. The host allows only the hosts its operator listed.
    ///
    /// # Errors
    ///
    /// Returns an error if the room has no browser, the URL is not allowed, or the visit fails.
    pub async fn browse(
        &self,
        url: &str,
        mode: browse::Mode,
        wait_ms: Option<u64>,
    ) -> Result<browse::Browsed> {
        let t = self
            .send_task(
                "browse",
                &Op::Browse {
                    url: url.to_string(),
                    mode,
                    wait_ms,
                },
                vec![],
            )
            .await?;
        let m = |k: &str| wire::meta(&t, k).cloned().unwrap_or(Value::Null);
        Ok(browse::Browsed {
            url: m("url").as_str().unwrap_or_default().to_string(),
            title: m("title").as_str().unwrap_or_default().to_string(),
            status: m("status").as_u64().and_then(|s| u16::try_from(s).ok()),
            text: wire::artifact_text(&t, "text"),
            html: wire::artifact_text(&t, "html"),
            screenshot: wire::artifact_bytes(&t, "screenshot"),
            blocked: m("blocked")
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            requests: m("requests").as_u64().unwrap_or(0) as usize,
            timed_out: m("timed_out").as_bool().unwrap_or(false),
            truncated: m("truncated").as_bool().unwrap_or(false),
        })
    }

    /// Fetch `refname` from the room; returns its commit id and a git bundle.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub async fn pull(&self, refname: &str) -> Result<(String, Vec<u8>)> {
        let t = self
            .send_task(
                "pull",
                &Op::Pull {
                    refname: refname.to_string(),
                },
                vec![],
            )
            .await?;
        let head = wire::meta(&t, "head")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let bundle =
            wire::artifact_bytes(&t, "bundle").ok_or_else(|| anyhow!("pull returned no bundle"))?;
        Ok((head, bundle))
    }

    /// Send a bundle holding `branch`. Returns the ref it was stored under on the host.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn push(&self, branch: &str, bundle: &[u8]) -> Result<String> {
        let part = Part::raw(bundle.to_vec())
            .with_filename("push.bundle")
            .with_media_type(wire::MEDIA_BUNDLE);
        let t = self
            .send_task(
                "push",
                &Op::Push {
                    branch: branch.to_string(),
                },
                vec![part],
            )
            .await?;
        Ok(wire::meta(&t, "reference")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Run a task on a checkout of `base`; returns the runner name, its output and the patch.
    /// A command that exits non-zero or times out still returns `Ok`; check `exit_code` / `timed_out`.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub async fn run_task(
        &self,
        base: &str,
        spec: TaskSpec,
    ) -> Result<(String, TaskOutput, Vec<u8>)> {
        let r = self.run_task_full(base, spec).await?;
        Ok((r.runner, r.output, r.patch))
    }

    /// Like [`Client::run_task`], with the task id and workspace commit details.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is bounded, or truncated on purpose"
    )]
    pub async fn run_task_full(&self, base: &str, spec: TaskSpec) -> Result<TaskRun> {
        let op = Op::Run {
            base: base.to_string(),
            spec,
        };
        let t = match self
            .send(&op, vec![])
            .await
            .map_err(|e| anyhow!("task refused: {e:#}"))?
        {
            SendMessageResponse::Task(t) => t,
            SendMessageResponse::Message(m) => {
                bail!("unexpected message instead of a task: {:?}", m.text())
            }
        };
        let exit_code = wire::meta(&t, "exitCode")
            .and_then(Value::as_i64)
            .map(|c| c as i32);
        let timed_out = wire::meta(&t, "timedOut")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let ran = t.status.state == TaskState::Completed || exit_code.is_some() || timed_out;
        if !ran {
            bail!("task refused: {}", wire::status_text(&t));
        }
        let text = |k: &str| {
            wire::meta(&t, k)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Ok(TaskRun {
            task_id: t.id.clone(),
            runner: text("runner").unwrap_or_default(),
            output: TaskOutput {
                exit_code,
                timed_out,
                stdout: wire::artifact_text(&t, "stdout").unwrap_or_default(),
                stderr: wire::artifact_text(&t, "stderr").unwrap_or_default(),
                policy_digest: text("policyDigest"),
                secrets_used: wire::meta(&t, "secretsUsed")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default(),
            },
            patch: wire::artifact_bytes(&t, "patch").unwrap_or_default(),
            branch: text("branch"),
            commit: text("commit"),
            changed: wire::meta(&t, "changed")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
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
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub async fn join_link(
    link: &str,
    secret: SecretKey,
    transport: impl Into<TransportChoice>,
) -> Result<Client> {
    let link = Link::parse(link)?;
    let transport = transport.into().resolve(link.transport.as_ref());
    let client = Client::connect(link.addr, secret, transport).await?;
    client.join(&link.invite).await?;
    Ok(client)
}

fn conv_field(v: &Value, field: &str) -> Result<Value> {
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        bail!("{e}");
    }
    Ok(v.get(field).cloned().unwrap_or(Value::Null))
}

fn call_of(v: &Value, host: EndpointId) -> Result<capabilities::Call> {
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        bail!("{e}");
    }
    let call: capabilities::Call =
        serde_json::from_value(v.get("call").cloned().unwrap_or(Value::Null))?;
    call.verify_result(host.as_bytes())?;
    Ok(call)
}

#[cfg(test)]
mod say_retry_tests {
    use super::*;
    use crate::{new_secret_key, Host, HostConfig};

    #[tokio::test]
    async fn say_survives_a_dropped_connection_by_retrying_once_on_a_new_one() {
        let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig::default())
            .await
            .unwrap();
        let link = host
            .create_link(Duration::from_secs(60), 1)
            .unwrap()
            .encode();
        let a = join_link(&link, new_secret_key().unwrap(), true)
            .await
            .unwrap();
        let before = a.say("one", None).await.unwrap();
        // Kill the connection under the client; the next say must still go through, exactly once.
        a.conn.close(0u32.into(), b"test");
        let after = a.say("two", None).await.unwrap();
        assert!(after > before);
        let b = a.sibling(a.addr.clone()).await.unwrap();
        let texts: Vec<String> = b
            .feed(0)
            .await
            .unwrap()
            .items
            .into_iter()
            .filter(|i| i.kind == "say")
            .map(|i| i.text)
            .collect();
        assert_eq!(texts, ["one", "two"]);
        // A refusal from the host is final and not retried: an empty message says nothing and costs no extra posts.
        assert!(b.say("   ", None).await.is_err());
        host.shutdown().await.unwrap();
    }
}
