//! The backend trait, its in-memory implementation and the request and result types.

use crate::sync::LockExt;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Accepted,
    Quarantined,
}

impl Status {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Accepted => "accepted",
            Status::Quarantined => "quarantined",
        }
    }
}

/// One entry in the index, as the host sends it.
#[derive(Debug, Clone)]
pub struct Item {
    pub doc: String,
    pub text: String,
    pub tags: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

/// A recall, as the host sends it.
#[derive(Debug, Clone)]
pub struct RecallRequest {
    pub query: String,
    pub max_tokens: u32,
    /// Every tag must be on a hit.
    pub tags: Vec<String>,
}

/// A hit, as the server returns it.
#[derive(Debug, Clone, Default)]
pub struct RawHit {
    pub doc: Option<String>,
    pub text: String,
    pub tags: Vec<String>,
    /// The server's kind of memory ("world", "experience", "observation").
    pub kind: Option<String>,
}

#[async_trait]
pub trait Backend: Send + Sync {
    async fn retain(&self, bank: &str, item: &Item) -> Result<()>;
    async fn recall(&self, bank: &str, req: &RecallRequest) -> Result<Vec<RawHit>>;
    /// Delete a document and what was derived from it. A document the server does not have is not an error.
    async fn delete(&self, bank: &str, doc: &str) -> Result<()>;
}

// ---- Hindsight over HTTP ----

/// Keeps entries in memory and recalls by shared words. A stand-in with the same contract, not a memory server.
#[derive(Default)]
pub struct InMemory {
    banks: Mutex<HashMap<String, BTreeMap<String, Item>>>,
    /// Count of calls to `delete`, so tests can see a revocation reach the index.
    pub deletes: Mutex<Vec<String>>,
}

impl InMemory {
    #[must_use]
    pub fn documents(&self, bank: &str) -> Vec<String> {
        self.banks
            .lock_recover()
            .get(bank)
            .map(|b| b.keys().cloned().collect())
            .unwrap_or_default()
    }
}

pub(super) fn words(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_lowercase)
        .collect()
}

#[async_trait]
impl Backend for InMemory {
    async fn retain(&self, bank: &str, item: &Item) -> Result<()> {
        self.banks
            .lock_recover()
            .entry(bank.to_string())
            .or_default()
            .insert(item.doc.clone(), item.clone());
        Ok(())
    }

    async fn recall(&self, bank: &str, req: &RecallRequest) -> Result<Vec<RawHit>> {
        let q = words(&req.query);
        let g = self.banks.lock_recover();
        let mut hits: Vec<(usize, RawHit)> = g
            .get(bank)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|i| req.tags.iter().all(|t| i.tags.contains(t)))
            .filter_map(|i| {
                let n = words(&i.text).intersection(&q).count();
                (n > 0).then(|| {
                    (
                        n,
                        RawHit {
                            doc: Some(i.doc.clone()),
                            text: i.text.clone(),
                            tags: i.tags.clone(),
                            kind: Some("world".into()),
                        },
                    )
                })
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.doc.cmp(&b.1.doc)));
        Ok(hits.into_iter().map(|h| h.1).collect())
    }

    async fn delete(&self, bank: &str, doc: &str) -> Result<()> {
        self.deletes.lock_recover().push(doc.to_string());
        if let Some(b) = self.banks.lock_recover().get_mut(bank) {
            b.remove(doc);
        }
        Ok(())
    }
}

// ---- the host's side ----
