//! A Model Context Protocol server (stdio, JSON-RPC 2.0) that lets any MCP-capable agent host or
//! join a Playbox room. One process is one sidecar: it can host a room, join one as a guest, or both.
//!
//! Every tool call and its result are appended to a signed, hash-chained history log.

// The tool schemas are one large `json!` literal each; `playbox_create` has enough options to pass the default limit of 128.
#![recursion_limit = "256"]

use anyhow::Result;
use ed25519_dalek::SigningKey;
use playbox_core::{memory::MemoryStore, taint::Broker};
use playbox_net::{Client, Host};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};

const FALLBACK_PROTOCOL: &str = "2025-06-18";
const SHOWN_PATCH_BYTES: usize = 20_000;

/// What the last refused call was waiting on, for a front end that can ask a person (see `playbox-rap`).
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub action: String,
    pub args: Vec<String>,
    /// Where the copied text came from.
    pub origins: Vec<String>,
}

/// Where a room's feed can be read without the server: the host itself, or a guest's connection.
#[derive(Clone)]
pub enum FeedSource {
    Host(Arc<Host>),
    Guest { client: Arc<Client>, me: String },
}

impl FeedSource {
    /// This member's id in the feed (`host` for the host).
    pub fn me(&self) -> &str {
        match self {
            FeedSource::Host(_) => "host",
            FeedSource::Guest { me, .. } => me,
        }
    }

    pub async fn feed(&self, since: u64) -> Result<playbox_net::chat::Feed> {
        match self {
            FeedSource::Host(h) => Ok(h.feed(since)),
            FeedSource::Guest { client, .. } => client.feed(since).await,
        }
    }
}

pub mod live;

pub struct Server {
    live: live::Live,
    pending: Option<PendingApproval>,
    host: Option<Arc<Host>>,
    host_repo: Option<PathBuf>,
    room_name: Option<String>,
    is_replica: bool,
    runtime_name: Option<String>,
    client: Option<Arc<Client>>,
    /// Other hosts of the room this guest has connected to (replicas), with the same grant.
    extra: Vec<Arc<Client>>,
    /// Which of `extra` serves calls; `None` means the primary in `client`.
    active: Option<usize>,
    member_id: Option<String>,
    /// The arguments of `playbox_join`, kept so a reconnected host is vetted the way the first connection was.
    join_args: Option<Value>,
    workspace: Option<PathBuf>,
    log_path: PathBuf,
    key: SigningKey,
    /// Admission broker: text that came from other members is tracked, and actions that copy it need
    /// a human's capability. Enforcing only when `$PLAYBOX_APPROVERS` names at least one approver key;
    /// otherwise it only warns.
    guard: Broker,
    enforce: bool,
    /// Shared memory with provenance, persisted as a JSON array of signed events in `memory_path`.
    memory: MemoryStore,
    memory_path: PathBuf,
}

const DEFAULT_STEER_IMAGE: &str = "playbox-steer:0.1";

mod args;
mod chat;
mod core;
mod memory;
mod proofs;
mod room;
mod tools;
mod work;
pub use tools::{tool_list, ROOM_CARD_URI};

#[cfg(test)]
mod tests {
    use crate::args::{abuse_from_args, latent_from_args};
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn room_limits_from_tool_arguments_match_playbox_up() {
        let d = playbox_net::limits::AbuseLimits::default();
        let none = abuse_from_args(&json!({}));
        assert_eq!(
            (
                none.max_members,
                none.max_concurrent_per_member,
                none.max_tasks_per_window,
                none.task_budget
            ),
            (
                d.max_members,
                d.max_concurrent_per_member,
                d.max_tasks_per_window,
                d.task_budget
            )
        );
        let l = abuse_from_args(
            &json!({"max_members": 3, "max_concurrent": 1, "max_tasks_per_minute": 5, "task_budget": 40}),
        );
        assert_eq!(
            (
                l.max_members,
                l.max_concurrent_per_member,
                l.max_tasks_per_window,
                l.task_budget
            ),
            (3, 1, 5, 40)
        );
        // Zero, negative and non-numbers keep the default rather than turning a limit off.
        for bad in [json!(0), json!(-1), json!("many"), json!(null), json!(1.5)] {
            assert_eq!(
                abuse_from_args(&json!({ "max_members": bad })).max_members,
                d.max_members
            );
        }
    }

    #[test]
    fn latent_policy_from_tool_arguments_match_playbox_up() {
        let d = playbox_net::latent::LatentPolicy::default();
        let none = latent_from_args(&json!({}));
        assert!(none.hubs.is_none());
        assert_eq!(
            (none.posts_per_window, none.max_age),
            (d.posts_per_window, d.max_age)
        );
        let l = latent_from_args(
            &json!({"latent_hubs": ["aa", "bb"], "latent_posts_per_minute": 4, "latent_max_age_secs": 90}),
        );
        assert_eq!(l.hubs, Some(vec!["aa".to_string(), "bb".to_string()]));
        assert_eq!(
            (l.posts_per_window, l.max_age),
            (4, Duration::from_secs(90))
        );
        // An empty or non-text list means "any hub", not "no hub": an empty allowlist must not lock the room out silently.
        assert!(latent_from_args(&json!({"latent_hubs": []})).hubs.is_none());
        assert!(latent_from_args(&json!({"latent_hubs": [1, null]}))
            .hubs
            .is_none());
        assert_eq!(
            latent_from_args(&json!({"latent_max_age_secs": 0})).max_age,
            d.max_age
        );
    }
}
