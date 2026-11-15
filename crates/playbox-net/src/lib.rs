//! Playbox networking: a host that admits members by invite and a client that joins by link.
//!
//! Peers speak A2A v1 JSON-RPC over iroh streams; see [`wire`] for the binding.

pub mod audit;
mod bind;
pub mod browse;
pub mod capabilities;
pub mod chat;
mod client;
pub mod conversations;
pub mod grants;
mod handler;
mod host;
mod keys;
pub mod latent;
pub mod limits;
mod link;
mod op_error;
pub mod review;
pub mod revoke;
pub mod shared_memory;
mod state;
pub mod steer;
pub mod transport;
pub mod wire;
pub mod ws;
pub use client::{join_link, Client, TaskRun};
pub(crate) use handler::{finish, refuse, unauthorized, RoomHandler};
pub use host::{Host, HostConfig};
pub use keys::{identity_key, new_secret_key, random_bytes};
pub use link::{decode_peer, encode_peer, key_for_link, Link, LinkOpts};
pub use revoke::{Notice, SYNC_ALPN};
pub(crate) use state::{write_atomic, State};
pub use transport::{Transport, TransportChoice};

pub use iroh::EndpointId;
use std::time::Duration;

pub const ALPN: &[u8] = b"playbox/a2a/1";
/// Cap for one request or response. Bundles and patches travel base64-encoded inside it, so the
/// largest usable payload is about three quarters of this.
const MAX_FRAME: usize = 256 * 1024 * 1024;
/// The most a host will read from a connection that has not yet proved anything: the first request on
/// a connection is at most this big. After one request succeeds with an invite or a grant, the connection
/// may send up to `MAX_FRAME`. Clients with a large first request send a tiny one first (see `Client`).
pub const MAX_UNAUTHENTICATED_REQUEST: usize = 64 * 1024;
/// How many requests one connection may make before one of them succeeds with an invite or a grant. Each refused
/// request is written to the audit log and the feed, so without a bound a stranger could fill the host's disk over a
/// single open connection. An honest client needs a handful (card, attestation, join).
pub const MAX_UNAUTHENTICATED_REQUESTS: u32 = 32;
/// How long a connection that has proved something may sit idle, or take to send one request.
const AUTHENTICATED_PATIENCE: Duration = Duration::from_secs(15 * 60);
/// Tasks remembered per member for `GetTask` / `ListTasks`.
const TASKS_KEPT: usize = 200;
const CONNECT_ATTEMPTS: u32 = 4;
const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(6);
const LINK_SCHEME: &str = "playbox://";
