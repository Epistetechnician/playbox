//! Typed op failures. `Display` is the exact text clients already receive in
//! `{"error": "..."}`, so the wire format is unchanged.

use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub(crate) enum OpError {
    #[error("this room has no shared memory")]
    NoSharedMemory,
    #[error("no such call")]
    NoSuchCall,
    #[error("{0}")]
    Failed(String),
}

impl OpError {
    pub(crate) fn failed(e: impl std::fmt::Display) -> Self {
        Self::Failed(format!("{e:#}"))
    }

    /// The wire body for this error.
    pub(crate) fn body(&self) -> String {
        json!({ "error": self.to_string() }).to_string()
    }
}
