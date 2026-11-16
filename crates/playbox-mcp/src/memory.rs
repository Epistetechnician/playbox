//! Shared memory: remembering, recalling, ingesting and exporting.

use crate::args::{parse_kind, req, s};
use crate::Server;
use anyhow::{anyhow, Context, Result};
use playbox_core::{
    memory::{NewEntry, Status},
    record::{hex, unhex},
    store,
};
use serde_json::{json, Value};

impl Server {
    pub(crate) fn save_memory(&self) -> Result<()> {
        std::fs::write(&self.memory_path, serde_json::to_vec(self.memory.log())?)
            .context("writing memory.json")
    }

    pub(crate) fn remember(&mut self, a: &Value) -> Result<Value> {
        let kind = parse_kind(req(a, "kind")?)?;
        let list = |k: &str| -> Vec<String> {
            a.get(k)
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        let (evidence, derived) = (list("evidence"), list("derived_from"));
        let (ev, dv): (Vec<&str>, Vec<&str>) = (
            evidence.iter().map(String::as_str).collect(),
            derived.iter().map(String::as_str).collect(),
        );
        let text = playbox_core::memory::Entry::sign(
            NewEntry {
                kind,
                topic: req(a, "topic")?,
                body: req(a, "body")?,
                evidence: &ev,
                derived_from: &dv,
                supersedes: s(a, "supersedes"),
                time_ms: store::now_ms(),
            },
            &self.memory.head(),
            &self.key,
        )
        .map_err(|e| anyhow!(e))?;
        let id = self
            .memory
            .apply(&text)
            .map_err(|e| anyhow!("memory refused: {e}"))?;
        self.save_memory()?;
        Ok(json!({"id": id, "status": format!("{:?}", self.memory.status(&id))}))
    }

    pub(crate) fn strings(a: &Value, k: &str) -> Vec<String> {
        a.get(k)
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn keys(a: &Value, k: &str) -> Result<Vec<[u8; 32]>> {
        Self::strings(a, k)
            .iter()
            .map(|h| unhex::<32>(h).map_err(|e| anyhow!("{k}: {h:?}: {e}")))
            .collect()
    }

    pub(crate) fn memory_ingest(&mut self, a: &Value) -> Result<Value> {
        let mut applied = Vec::new();
        let mut refused = Vec::new();
        for (i, ev) in Self::strings(a, "events").iter().enumerate() {
            match self.memory.apply(ev) {
                Ok(id) => applied.push(json!({"event": i, "entry": id, "status": self.memory.status(&id).map(|s| format!("{s:?}"))})),
                Err(e) => refused.push(json!({"event": i, "why": e.to_string()})),
            }
        }
        self.save_memory()?;
        Ok(json!({"applied": applied, "refused": refused}))
    }

    pub(crate) fn memory_export(&self) -> Result<Value> {
        let pending: Vec<Value> = self
            .memory
            .log()
            .iter()
            .filter_map(|t| self.memory.entry_for_event(t))
            .filter(|e| self.memory.status(&e.id) == Some(Status::Quarantined))
            .map(
                |e| json!({"id": e.id, "topic": e.topic, "body": e.body, "author": hex(&e.author)}),
            )
            .collect();
        Ok(json!({"events": self.memory.log(), "quarantined": pending, "head": self.memory.head()}))
    }

    pub(crate) fn recall(&mut self, a: &Value) -> Result<Value> {
        let kind = s(a, "kind").map(parse_kind).transpose()?;
        let entries: Vec<Value> = self
            .memory
            .recall(kind, &mut self.guard.tracker)
            .into_iter()
            .map(|e| json!({"id": e.id, "topic": e.topic, "body": e.body, "evidence": e.evidence, "derived_from": e.derived_from}))
            .collect();
        let conflicts: Vec<Value> = self
            .memory
            .conflicts()
            .into_iter()
            .map(|(x, y)| json!([x, y]))
            .collect();
        Ok(json!({"entries": entries, "conflicts": conflicts}))
    }
}
