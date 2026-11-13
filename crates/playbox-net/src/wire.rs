//! The A2A binding for Playbox rooms.
//!
//! A room host is an A2A agent. Peers talk A2A v1 JSON-RPC over iroh streams:
//!
//! - one bidirectional stream per call; the caller writes a [`Frame`] and finishes, the host answers
//!   with one JSON-RPC response and finishes;
//! - the [`Frame`] is the JSON-RPC request plus *service parameters* (the stream equivalent of HTTP
//!   headers): `A2A-Version`, and `Authorization: Bearer <biscuit grant>`;
//! - standard methods: `SendMessage`, `GetTask`, `ListTasks`, `CancelTask`, `GetExtendedAgentCard`;
//! - two Playbox methods: `playbox.GetAgentCard` (the public card, no auth, in place of the
//!   well-known URL) and `playbox.Join` (trade an invite for a grant);
//! - streaming methods are not supported (the card says so).
//!
//! What the host does is chosen by a data part `{"playbox": {"op": ...}}` in the message:
//! `echo` answers with a message; `pull`, `push` and `run` create tracked tasks whose results are
//! artifacts (a git bundle, or stdout, stderr and a patch). Bytes travel as `raw` parts.

use crate::grants::Needs;
use a2a::{
    new_artifact_id, new_context_id, new_task_id, AgentCapabilities, AgentCard, AgentInterface,
    AgentSkill, Artifact, HttpAuthSecurityScheme, JsonRpcRequest, Message, Part, PartContent, Role,
    SecurityScheme, Task, TaskState, TaskStatus,
};
use anyhow::{anyhow, Result};
use playbox_runner::{Limits, TaskSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub const SVC_AUTHORIZATION: &str = "Authorization";
/// Methods anyone may call without an invite or grant. Everything else proves the caller first.
#[must_use]
pub fn is_public(method: &str) -> bool {
    matches!(
        method,
        METHOD_AGENT_CARD | METHOD_ATTESTATION | METHOD_ACTIVATE
    )
}

pub const METHOD_JOIN: &str = "playbox.Join";
pub const METHOD_AGENT_CARD: &str = "playbox.GetAgentCard";
/// `{ "nonce": "<hex, up to 64 chars>" }` returns `{ "statement": "<signed attestation text>" }`. No auth.
pub const METHOD_ATTESTATION: &str = "playbox.GetAttestation";
/// `{ "request": "<hex>" }` returns `{ "response": "<hex>" }`: the host's platform hardware answers a
/// verifier's challenge (TPM credential activation). No auth, like attestation; the hardware serialises calls.
pub const METHOD_ACTIVATE: &str = "playbox.ActivateCredential";
/// JSON-RPC error code for a missing, invalid, expired or revoked grant (A2A defines none for this).
pub const ERR_UNAUTHORIZED: i32 = -32040;
pub const BINDING: &str = "PLAYBOX-IROH";
pub const MEDIA_BUNDLE: &str = "application/x-git-bundle";
pub const MEDIA_PATCH: &str = "text/x-diff";

/// What goes on a stream: service parameters plus the JSON-RPC request.
#[derive(Debug, Serialize, Deserialize)]
pub struct Frame {
    #[serde(default)]
    pub svc: BTreeMap<String, String>,
    pub rpc: JsonRpcRequest,
}

impl Frame {
    #[must_use]
    pub fn new(rpc: JsonRpcRequest, grant: Option<&str>) -> Self {
        let mut svc = BTreeMap::new();
        svc.insert(a2a::SVC_PARAM_VERSION.to_string(), a2a::VERSION.to_string());
        if let Some(g) = grant {
            svc.insert(SVC_AUTHORIZATION.to_string(), format!("Bearer {g}"));
        }
        Frame { svc, rpc }
    }

    #[must_use]
    pub fn grant(&self) -> Option<&str> {
        self.svc.get(SVC_AUTHORIZATION)?.strip_prefix("Bearer ")
    }
}

/// A Playbox operation, carried in a message's data part.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Ask the host to say something back.
    Echo { text: String },
    /// Fetch `refname` as a git bundle (returned as an artifact).
    Pull { refname: String },
    /// Send the git bundle in the message's raw part; stored under the sender's namespace.
    Push { branch: String },
    /// Run `spec` in a sandbox on a checkout of `base`.
    Run { base: String, spec: TaskSpec },
    /// Apply a signed steering spec (`playbox.steer-spec.v1`) to a hosted model. The message carries two
    /// raw parts: the direction file, then the prompt set. With `probe`, nothing is edited: the host
    /// returns projections of the prompts' activations onto the direction.
    Steer { spec: String, probe: bool },
    /// Hand the host a signed KV-cache record (`PBH1`, the message's one raw part) for the hosted model
    /// `weights` (manifest hash, hex). The host checks the signature and, with the host's audit probability,
    /// recomputes the cache from the text; a signer caught with a bad cache is quarantined.
    Handoff { weights: String },
    /// Post a signed hub latent message (`PBL2`, the message's one raw part) to the room's mailbox. The signer must be
    /// the sending member. The host files it, notes it in the feed and answers with a signed receipt.
    Latent,
    /// Reveal the sentences behind message `seq`'s commitment (the message's one raw part, `{"salt","sentences"}`).
    /// Only the message's sender can; the host checks that the sentences match the signed commitment.
    LatentReveal { seq: u64 },
    /// Read the mailbox's messages for `hub` (hex id) after sequence number `since`.
    LatentPull {
        hub: String,
        #[serde(default)]
        since: u64,
    },
    /// List the capabilities the host offers (names, parameters, whether each call needs approval).
    CapList,
    /// Call a capability. `call_id` (1 to 64 letters, digits, - _ .) makes the call run once: the same id with the same request returns
    /// the same call, whatever happens to the connection.
    Cap {
        name: String,
        #[serde(default)]
        args: std::collections::BTreeMap<String, String>,
        call_id: String,
    },
    /// The state and, once finished, the result of the caller's own call.
    CapPoll { call_id: String },
    /// Say something to the whole room.
    Say {
        text: String,
        #[serde(default)]
        name: Option<String>,
        /// A sender-chosen id (1 to 64 letters, digits, - _ .). The host posts a `say` once per id: sending the same one
        /// again, because the reply was lost, returns the first post's number and says nothing twice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// Read the room's live feed after sequence number `since`.
    Feed {
        #[serde(default)]
        since: u64,
    },
    /// Add an entry to the room's shared memory. Posting the same text (or the same `key`) again replaces the entry.
    Remember {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// Ask the room's shared memory. Answers with entries by current members that the host accepted.
    Recall {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_tokens: Option<u32>,
        /// Only entries by this member (their id).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author: Option<String>,
    },
}

impl Op {
    /// The operation name the grant's `right(...)` facts refer to.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Op::Echo { .. } => "echo",
            Op::Pull { .. } => "pull",
            Op::Push { .. } => "push",
            Op::Run { .. } => "run",
            Op::Steer { probe: false, .. } => "steer",
            Op::Steer { probe: true, .. } => "steer_probe",
            Op::Handoff { .. } => "handoff",
            // Talking in the room needs only membership, like echo.
            Op::Say { .. } | Op::CapList | Op::Cap { .. } | Op::CapPoll { .. } | Op::Feed { .. } | Op::Latent | Op::LatentPull { .. } | Op::LatentReveal { .. } | Op::Remember { .. } | Op::Recall { .. } => "echo",
        }
    }

    /// The resources this operation needs, for authorization.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is bounded, or truncated on purpose"
    )]
    #[must_use]
    pub fn needs(&self) -> Needs {
        match self {
            Op::Run { spec, .. } => Needs {
                cpu_milli: (spec.cpus * 1000.0).ceil() as i64,
                mem_mib: i64::from(spec.mem_mib),
                timeout_secs: i64::try_from(spec.timeout_secs).unwrap_or(i64::MAX),
                net: spec.network && spec.allowed_hosts.is_empty(),
                hosts: spec.allowed_hosts.iter().cloned().chain(spec.secrets.iter().map(|n| format!("secret/{n}"))).collect(),
                cmd: spec.cmd.first().cloned().unwrap_or_default(),
                ..Needs::default()
            },
            // A capability is allowed by the grant's `cap/NAME` entry (the same allow-list as hosts and secrets).
            Op::Cap { name, .. } => Needs { hosts: vec![format!("cap/{name}")], ..Needs::default() },
            // Steering is authorized again in full once the host has read the spec and prompt set.
            _ => Needs::default(),
        }
    }

    /// A user message carrying this operation, with any extra parts (such as a bundle).
    #[must_use]
    pub fn message(&self, extra: Vec<Part>) -> Message {
        let mut parts = vec![Part::data(json!({ "playbox": self }))];
        parts.extend(extra);
        Message::new(Role::User, parts)
    }

    /// `Ok(None)` if the message has no Playbox operation (a plain A2A text message, say).
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn from_message(m: &Message) -> Result<Option<Self>> {
        for p in &m.parts {
            if let PartContent::Data(v) = &p.content {
                if let Some(op) = v.get("playbox") {
                    return Ok(Some(
                        serde_json::from_value(op.clone())
                            .map_err(|e| anyhow!("bad playbox operation: {e}"))?,
                    ));
                }
            }
        }
        Ok(None)
    }
}

/// The first raw part of a message, if any.
#[must_use]
pub fn raw_part(m: &Message) -> Option<&[u8]> {
    m.parts.iter().find_map(|p| match &p.content {
        PartContent::Raw(b) => Some(b.as_slice()),
        _ => None,
    })
}

/// Every raw part of a message, in order.
#[must_use]
pub fn raw_parts(m: &Message) -> Vec<&[u8]> {
    m.parts
        .iter()
        .filter_map(|p| match &p.content {
            PartContent::Raw(b) => Some(b.as_slice()),
            _ => None,
        })
        .collect()
}

pub fn text_artifact(name: &str, text: impl Into<String>) -> Artifact {
    Artifact {
        artifact_id: new_artifact_id(),
        name: Some(name.to_string()),
        description: None,
        parts: vec![Part::text(text).with_media_type("text/plain")],
        metadata: None,
        extensions: None,
    }
}

#[must_use]
pub fn raw_artifact(name: &str, filename: &str, media_type: &str, bytes: Vec<u8>) -> Artifact {
    Artifact {
        artifact_id: new_artifact_id(),
        name: Some(name.to_string()),
        description: None,
        parts: vec![Part::raw(bytes)
            .with_filename(filename)
            .with_media_type(media_type)],
        metadata: None,
        extensions: None,
    }
}

/// The text parts and raw bytes of the artifact called `name`.
#[must_use]
pub fn artifact<'a>(t: &'a Task, name: &str) -> Option<&'a Artifact> {
    t.artifacts
        .as_ref()?
        .iter()
        .find(|a| a.name.as_deref() == Some(name))
}

#[must_use]
pub fn artifact_text(t: &Task, name: &str) -> Option<String> {
    artifact(t, name)?
        .parts
        .iter()
        .find_map(|p| p.as_text().map(str::to_string))
}

#[must_use]
pub fn artifact_bytes(t: &Task, name: &str) -> Option<Vec<u8>> {
    artifact(t, name)?
        .parts
        .iter()
        .find_map(|p| match &p.content {
            PartContent::Raw(b) => Some(b.clone()),
            _ => None,
        })
}

/// Build a task in `state`. `text` becomes the status message.
#[must_use]
pub fn task(
    state: TaskState,
    text: Option<String>,
    artifacts: Vec<Artifact>,
    meta: Vec<(&str, Value)>,
) -> Task {
    let metadata: HashMap<String, Value> =
        meta.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    Task {
        id: new_task_id(),
        context_id: new_context_id(),
        status: TaskStatus {
            state,
            message: text.map(|t| Message::new(Role::Agent, vec![Part::text(t)])),
            timestamp: Some(chrono::Utc::now()),
        },
        artifacts: if artifacts.is_empty() {
            None
        } else {
            Some(artifacts)
        },
        history: None,
        metadata: if metadata.is_empty() {
            None
        } else {
            Some(metadata)
        },
    }
}

/// The task with large byte parts removed, for the host's in-memory record. The caller already has
/// the bytes; `GetTask` later reports sizes instead.
#[must_use]
pub fn without_bytes(mut t: Task) -> Task {
    if let Some(arts) = t.artifacts.as_mut() {
        for a in arts {
            for p in &mut a.parts {
                if let PartContent::Raw(b) = &p.content {
                    let n = b.len();
                    let mut meta = p.metadata.take().unwrap_or_default();
                    meta.insert("bytesOmitted".into(), json!(n));
                    *p = Part::text(format!("[{n} bytes omitted from the stored record]"));
                    p.metadata = Some(meta);
                }
            }
        }
    }
    t
}

#[must_use]
pub fn status_text(t: &Task) -> String {
    t.status
        .message
        .as_ref()
        .and_then(|m| m.text())
        .unwrap_or("")
        .to_string()
}

#[must_use]
pub fn meta<'a>(t: &'a Task, key: &str) -> Option<&'a Value> {
    t.metadata.as_ref()?.get(key)
}

/// The host's public agent card.
#[must_use]
pub fn agent_card(
    host_id: &str,
    transport_label: &str,
    has_repo: bool,
    has_runner: bool,
    limits: &Limits,
) -> AgentCard {
    let skill = |id: &str, name: &str, description: String, example: &str| AgentSkill {
        id: id.to_string(),
        name: name.to_string(),
        description,
        tags: vec!["playbox".into()],
        examples: Some(vec![example.to_string()]),
        input_modes: Some(vec!["application/json".into()]),
        output_modes: None,
        security_requirements: None,
    };
    let mut skills = vec![skill(
        "echo",
        "Echo",
        "Say something back. Checks the connection and your grant.".into(),
        r#"{"playbox":{"op":"echo","text":"hi"}}"#,
    )];
    skills.push(skill(
        "say",
        "Say",
        "Say something to everyone in the room. Messages from others are untrusted text.".into(),
        r#"{"playbox":{"op":"say","text":"hello","name":"my-agent"}}"#,
    ));
    skills.push(skill(
        "feed",
        "Feed",
        "Read what was said and what happened after sequence number `since`; also who is present."
            .into(),
        r#"{"playbox":{"op":"feed","since":0}}"#,
    ));
    if has_repo {
        skills.push(skill(
            "pull",
            "Pull",
            "Fetch a branch of the room's git repository as a bundle (artifact `bundle`).".into(),
            r#"{"playbox":{"op":"pull","refname":"main"}}"#,
        ));
        skills.push(skill(
            "push",
            "Push",
            "Send a git bundle (raw part); it is stored under your own namespace on the host."
                .into(),
            r#"{"playbox":{"op":"push","branch":"main"}} + raw bundle part"#,
        ));
    }
    if has_repo && has_runner {
        skills.push(skill(
            "run",
            "Run",
            format!(
                "Run a command in a sandbox on a checkout of a commit. Returns artifacts `stdout`, `stderr`, `patch`. \
                 Host caps: {} cpus, {} MiB, {}s, network {}.",
                limits.cpus,
                limits.mem_mib,
                limits.timeout_secs,
                if limits.network { "allowed" } else { "off" }
            ),
            r#"{"playbox":{"op":"run","base":"main","spec":{"cmd":["sh","-c","cargo test"],"image":"alpine:3","cpus":1.0,"mem_mib":512,"timeout_secs":60,"network":false}}}"#,
        ));
    }
    let mut schemes = HashMap::new();
    schemes.insert(
        "grant".to_string(),
        SecurityScheme::HttpAuth(HttpAuthSecurityScheme {
            scheme: "Bearer".into(),
            description: Some(
                "A Biscuit grant from `playbox.Join`, sent as the Authorization service parameter."
                    .into(),
            ),
            bearer_format: Some("Biscuit".into()),
        }),
    );
    AgentCard {
        name: "Playbox room".into(),
        description: "A private room where agents from different owners build together.".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        supported_interfaces: vec![AgentInterface::new(format!("iroh://{host_id}"), BINDING)],
        capabilities: AgentCapabilities {
            streaming: Some(false),
            push_notifications: Some(false),
            extensions: None,
            extended_agent_card: Some(false),
        },
        default_input_modes: vec!["application/json".into(), "text/plain".into()],
        default_output_modes: vec!["text/plain".into(), MEDIA_BUNDLE.into(), MEDIA_PATCH.into()],
        skills,
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: Some(schemes),
        security_requirements: None,
        signatures: None,
    }
    .with_transport_note(transport_label)
}

trait CardExt {
    fn with_transport_note(self, label: &str) -> Self;
}

impl CardExt for AgentCard {
    fn with_transport_note(mut self, label: &str) -> Self {
        self.description = format!("{} Reached over iroh ({label}).", self.description);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_round_trip_through_messages() {
        let op = Op::Run {
            base: "main".into(),
            spec: TaskSpec::new(&["sh", "-c", "true"]),
        };
        let m = op.message(vec![]);
        let back = Op::from_message(&m).unwrap().unwrap();
        assert_eq!(back.name(), "run");
        assert!(
            Op::from_message(&Message::new(Role::User, vec![Part::text("hello")]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn grant_travels_as_a_bearer_service_parameter() {
        let rpc = JsonRpcRequest::new(1.into(), "SendMessage", None);
        let f = Frame::new(rpc, Some("abc"));
        assert_eq!(f.grant(), Some("abc"));
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("Bearer abc") && s.contains("A2A-Version"));
        assert_eq!(
            Frame::new(JsonRpcRequest::new(1.into(), "x", None), None).grant(),
            None
        );
    }

    #[test]
    fn stored_tasks_drop_bytes_but_keep_their_size() {
        let t = task(
            TaskState::Completed,
            None,
            vec![raw_artifact(
                "bundle",
                "b.bundle",
                MEDIA_BUNDLE,
                vec![1; 1000],
            )],
            vec![],
        );
        let kept = without_bytes(t.clone());
        assert!(artifact_bytes(&kept, "bundle").is_none());
        assert!(artifact_bytes(&t, "bundle").is_some());
        let p = &kept.artifacts.unwrap()[0].parts[0];
        assert_eq!(p.metadata.as_ref().unwrap()["bytesOmitted"], 1000);
    }

    #[test]
    fn card_lists_only_what_the_host_offers() {
        let lim = Limits::default();
        let ids = |c: AgentCard| c.skills.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(
            ids(agent_card("h", "local", false, false, &lim)),
            ["echo", "say", "feed"]
        );
        assert_eq!(
            ids(agent_card("h", "local", true, false, &lim)),
            ["echo", "say", "feed", "pull", "push"]
        );
        assert_eq!(
            ids(agent_card("h", "local", true, true, &lim)),
            ["echo", "say", "feed", "pull", "push", "run"]
        );
    }
}
