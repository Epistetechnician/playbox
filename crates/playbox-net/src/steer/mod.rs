//! Hosting steering runs (see `docs/steering.md`).
//!
//! A member sends a signed [`SteerSpec`] plus two raw parts: the direction file and the prompt set. The
//! host checks that the spec is signed by the sender's own key, that both files hash to what the spec
//! names, that the model is one it hosts, and that the grant allows this model, these layers, this
//! strength, this mode, this direction and this many tokens. It then runs the pinned engine image with the
//! weights mounted read-only and no network, and answers with the engine's output and a [`SteerReceipt`]
//! signed by the host key.
//!
//! Weights live in a content-addressed store, `<store>/weights/<manifest hash>/`; the host recomputes each
//! hash when it opens the store and refuses to start on a mismatch. Safety guards sit beside them in
//! `<store>/guards/<manifest hash>/guards.json`.
//!
//! # What this does and does not prove
//! The receipt says this host ran this spec on these weights and prompts in this image and got this
//! output. It does not say the host is honest (see the receipt's [`Ceiling`]), that the engine image has no
//! bug, or that the direction means what its author says. Guards catch directions close to a known
//! safety direction; a novel one is not recognized.

mod client;
mod files;
mod host;
mod message;
#[cfg(test)]
mod tests;
pub use client::SteerResult;
pub(crate) use files::hex_to_64;
pub use files::{hex, verify_decision, weights_manifest, PromptSet};
pub use host::{SteerHost, SteerLimits};

/// Largest direction file (a 262144-wide model) and prompt set the host will take.
pub const MAX_DIRECTION_BYTES: usize = 1 << 20;
pub const MAX_PROMPT_BYTES: usize = 512 << 10;
/// Largest handoff envelope (record, text and KV cache) the host will take.
/// Largest hub size `k` a latent message may declare.
pub const MAX_HUB_K: usize = 4096;
pub const MAX_HANDOFF_BYTES: usize = 128 << 20;
/// Default chance that the host recomputes a handed-over cache from its text.
pub const DEFAULT_AUDIT_PROB: f64 = 0.1;
/// Largest engine output the host will read back.
const MAX_OUTPUT_BYTES: u64 = 8 << 20;

/// Container paths the engine sees. Fixed, so the command line carries nothing a member chose.
const WEIGHTS_AT: &str = "/weights";
const GUARDS_AT: &str = "/guards";
