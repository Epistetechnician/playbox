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
    new_artifact_id, new_context_id, new_task_id, AgentCapabilities, AgentCard, AgentInterface, AgentSkill,
    Artifact, HttpAuthSecurityScheme, JsonRpcRequest, Message, Part, PartContent, Role, SecurityScheme, Task,
    TaskState, TaskStatus,
};
use anyhow::{anyhow, Result};
use playbox_runner::{Limits, TaskSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub const SVC_AUTHORIZATION: &str = "Authorization";
pub const METHOD_JOIN: &str = "playbox.Join";
pub const METHOD_AGENT_CARD: &str = "playbox.GetAgentCard";
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
    pub fn new(rpc: JsonRpcRequest, grant: Option<&str>) -> Self {
        let mut svc = BTreeMap::new();
        svc.insert(a2a::SVC_PARAM_VERSION.to_string(), a2a::VERSION.to_string());
        if let Some(g) = grant {
            svc.insert(SVC_AUTHORIZATION.to_string(), format!("Bearer {g}"));
        }
        Frame { svc, rpc }
    }

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
}

impl Op {
    /// The operation name the grant's `right(...)` facts refer to.
    pub fn name(&self) -> &'static str {
        match self {
            Op::Echo { .. } => "echo",
            Op::Pull { .. } => "pull",
            Op::Push { .. } => "push",
            Op::Run { .. } => "run",
        }
    }

    /// The resources this operation needs, for authorization.
    pub fn needs(&self) -> Needs {
        match self {
            Op::Run { spec, .. } => Needs {
                cpu_milli: (spec.cpus * 1000.0).ceil() as i64,
                mem_mib: spec.mem_mib as i64,
                timeout_secs: spec.timeout_secs as i64,
                net: spec.network,
                cmd: spec.cmd.first().cloned().unwrap_or_default(),
            },
            _ => Needs::default(),
        }
    }

    /// A user message carrying this operation, with any extra parts (such as a bundle).
    pub fn message(&self, extra: Vec<Part>) -> Message {
        let mut parts = vec![Part::data(json!({ "playbox": self }))];
        parts.extend(extra);
        Message::new(Role::User, parts)
    }

    /// `Ok(None)` if the message has no Playbox operation (a plain A2A text message, say).
    pub fn from_message(m: &Message) -> Result<Option<Self>> {
        for p in &m.parts {
            if let PartContent::Data(v) = &p.content {
                if let Some(op) = v.get("playbox") {
                    return Ok(Some(serde_json::from_value(op.clone()).map_err(|e| anyhow!("bad playbox operation: {e}"))?));
                }
            }
        }
        Ok(None)
    }
}

/// The first raw part of a message, if any.
pub fn raw_part(m: &Message) -> Option<&[u8]> {
    m.parts.iter().find_map(|p| match &p.content {
        PartContent::Raw(b) => Some(b.as_slice()),
        _ => None,
    })
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

pub fn raw_artifact(name: &str, filename: &str, media_type: &str, bytes: Vec<u8>) -> Artifact {
    Artifact {
        artifact_id: new_artifact_id(),
        name: Some(name.to_string()),
        description: None,
        parts: vec![Part::raw(bytes).with_filename(filename).with_media_type(media_type)],
        metadata: None,
        extensions: None,
    }
}

/// The text parts and raw bytes of the artifact called `name`.
pub fn artifact<'a>(t: &'a Task, name: &str) -> Option<&'a Artifact> {
    t.artifacts.as_ref()?.iter().find(|a| a.name.as_deref() == Some(name))
}

pub fn artifact_text(t: &Task, name: &str) -> Option<String> {
    artifact(t, name)?.parts.iter().find_map(|p| p.as_text().map(str::to_string))
}

pub fn artifact_bytes(t: &Task, name: &str) -> Option<Vec<u8>> {
    artifact(t, name)?.parts.iter().find_map(|p| match &p.content {
        PartContent::Raw(b) => Some(b.clone()),
        _ => None,
    })
}

/// Build a task in `state`. `text` becomes the status message.
pub fn task(state: TaskState, text: Option<String>, artifacts: Vec<Artifact>, meta: Vec<(&str, Value)>) -> Task {
    let metadata: HashMap<String, Value> = meta.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    Task {
        id: new_task_id(),
        context_id: new_context_id(),
        status: TaskStatus {
            state,
            message: text.map(|t| Message::new(Role::Agent, vec![Part::text(t)])),
            timestamp: Some(chrono::Utc::now()),
        },
        artifacts: if artifacts.is_empty() { None } else { Some(artifacts) },
        history: None,
        metadata: if metadata.is_empty() { None } else { Some(metadata) },
    }
}

/// The task with large byte parts removed, for the host's in-memory record. The caller already has
/// the bytes; `GetTask` later reports sizes instead.
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

pub fn status_text(t: &Task) -> String {
    t.status.message.as_ref().and_then(|m| m.text()).unwrap_or("").to_string()
}

pub fn meta<'a>(t: &'a Task, key: &str) -> Option<&'a Value> {
    t.metadata.as_ref()?.get(key)
}

/// The host's public agent card.
pub fn agent_card(host_id: &str, transport_label: &str, has_repo: bool, has_runner: bool, limits: &Limits) -> AgentCard {
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
            "Send a git bundle (raw part); it is stored under your own namespace on the host.".into(),
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
            description: Some("A Biscuit grant from `playbox.Join`, sent as the Authorization service parameter.".into()),
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
        let op = Op::Run { base: "main".into(), spec: TaskSpec::new(&["sh", "-c", "true"]) };
        let m = op.message(vec![]);
        let back = Op::from_message(&m).unwrap().unwrap();
        assert_eq!(back.name(), "run");
        assert!(Op::from_message(&Message::new(Role::User, vec![Part::text("hello")])).unwrap().is_none());
    }

    #[test]
    fn grant_travels_as_a_bearer_service_parameter() {
        let rpc = JsonRpcRequest::new(1.into(), "SendMessage", None);
        let f = Frame::new(rpc, Some("abc"));
        assert_eq!(f.grant(), Some("abc"));
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("Bearer abc") && s.contains("A2A-Version"));
        assert_eq!(Frame::new(JsonRpcRequest::new(1.into(), "x", None), None).grant(), None);
    }

    #[test]
    fn stored_tasks_drop_bytes_but_keep_their_size() {
        let t = task(TaskState::Completed, None, vec![raw_artifact("bundle", "b.bundle", MEDIA_BUNDLE, vec![1; 1000])], vec![]);
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
        assert_eq!(ids(agent_card("h", "local", false, false, &lim)), ["echo"]);
        assert_eq!(ids(agent_card("h", "local", true, false, &lim)), ["echo", "pull", "push"]);
        assert_eq!(ids(agent_card("h", "local", true, true, &lim)), ["echo", "pull", "push", "run"]);
    }
}
