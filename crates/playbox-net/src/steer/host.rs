//! The steering host: the models it serves, per-member token budgets and the quarantine list.

use super::files::{hex, weights_manifest};
use super::DEFAULT_AUDIT_PROB;
use crate::sync::LockExt;
use crate::{grants::SteerCaps, EndpointId};
use anyhow::{bail, Context, Result};
use playbox_core::steer::Ceiling;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone)]
pub struct SteerLimits {
    pub cpus: f32,
    pub mem_mib: u32,
    pub timeout_secs: u64,
}

impl Default for SteerLimits {
    fn default() -> Self {
        Self {
            cpus: 2.0,
            mem_mib: 3072,
            timeout_secs: 600,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Usage {
    runs: u64,
    tokens: u64,
}

/// A host's steering configuration: which weights it serves, the engine image, the limits, and what it
/// issues to members.
pub struct SteerHost {
    pub(super) image: String,
    pub(super) limits: SteerLimits,
    caps: SteerCaps,
    pub(super) ceiling: Ceiling,
    /// Weights manifest hash (hex) -> directory.
    pub(super) weights: BTreeMap<String, PathBuf>,
    /// Weights manifest hash -> (guards directory, classes it defines).
    pub(super) guards: HashMap<String, (PathBuf, Vec<String>)>,
    usage: Mutex<HashMap<EndpointId, Usage>>,
    /// Chance, per handoff, of recomputing the cache. The engine draws it from the OS random source.
    pub(super) audit_prob: f64,
    /// Senders caught handing over a cache that was not computed from its text.
    pub(super) quarantined: Mutex<std::collections::HashSet<EndpointId>>,
    /// Where the quarantine is kept so it survives a restart.
    quarantine_file: PathBuf,
}

impl std::fmt::Debug for SteerHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteerHost")
            .field("image", &self.image)
            .field("models", &self.weights.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SteerHost {
    /// Open a store. Every `weights/<hash>` directory is hashed and must match its name. `caps.models` may
    /// be empty, meaning every model in the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn open(
        store: &Path,
        image: &str,
        limits: SteerLimits,
        mut caps: SteerCaps,
        ceiling: Ceiling,
    ) -> Result<Self> {
        let mut weights = BTreeMap::new();
        let wdir = store.join("weights");
        for e in std::fs::read_dir(&wdir)
            .with_context(|| format!("steer store {} has no weights directory", store.display()))?
        {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if !e.file_type()?.is_dir() || name.starts_with('.') {
                continue;
            }
            let got = hex(&weights_manifest(&e.path())?);
            if got != name {
                bail!(
                    "weights directory {name} hashes to {got}: the store is corrupt or mislabeled"
                );
            }
            weights.insert(name, e.path());
        }
        if weights.is_empty() {
            bail!("steer store {} holds no weights", store.display());
        }
        if caps.models.is_empty() {
            caps.models = weights.keys().cloned().collect();
        }
        for m in &caps.models {
            if !weights.contains_key(m) {
                bail!("grant allows model {m} but the store does not hold it");
            }
        }
        caps.validate()?;
        let mut guards = HashMap::new();
        for name in weights.keys() {
            let dir = store.join("guards").join(name);
            let file = dir.join("guards.json");
            if file.is_file() {
                let list: Vec<Value> = serde_json::from_slice(&std::fs::read(&file)?)
                    .with_context(|| format!("{} is not a JSON list", file.display()))?;
                let mut classes = Vec::new();
                for g in list {
                    let c = g.get("class").and_then(Value::as_str).unwrap_or_default();
                    if c.is_empty() || !c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                        bail!(
                            "{}: guard class {c:?} must be lowercase words",
                            file.display()
                        );
                    }
                    if !classes.iter().any(|x| x == c) {
                        classes.push(c.to_string());
                    }
                }
                guards.insert(name.clone(), (dir, classes));
            }
        }
        Ok(Self {
            image: image.to_string(),
            limits,
            caps,
            ceiling,
            weights,
            guards,
            usage: Mutex::new(HashMap::new()),
            audit_prob: DEFAULT_AUDIT_PROB,
            quarantined: Mutex::new(load_quarantine(&store.join("quarantine.json"))),
            quarantine_file: store.join("quarantine.json"),
        })
    }

    /// Set the chance that a handoff is audited by recomputation, in `[0, 1]`.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn with_audit_prob(mut self, p: f64) -> Result<Self> {
        if !(0.0..=1.0).contains(&p) {
            bail!("audit probability must be in [0, 1]");
        }
        self.audit_prob = p;
        Ok(self)
    }

    /// The grant's task caps come from the host's `Limits`; steering asks for its own. If steering needs more
    /// than the grant would ever allow, every request would be refused with a bare "not authorized", so
    /// fail at startup instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn check_against(&self, l: &playbox_runner::Limits) -> Result<()> {
        let s = &self.limits;
        if s.cpus > l.cpus || s.mem_mib > l.mem_mib || s.timeout_secs > l.timeout_secs {
            bail!(
                "steering needs {} cpus, {} MiB and {}s but the host's task limits are {} cpus, {} MiB and {}s: raise the limits or lower the steering limits",
                s.cpus, s.mem_mib, s.timeout_secs, l.cpus, l.mem_mib, l.timeout_secs
            );
        }
        Ok(())
    }

    /// What members of this room are issued at join.
    pub fn caps(&self) -> &SteerCaps {
        &self.caps
    }

    pub fn models(&self) -> Vec<String> {
        self.weights.keys().cloned().collect()
    }

    pub fn image(&self) -> &str {
        &self.image
    }

    /// Count a run against `who`'s budget, before it starts so a failing run cannot be repeated for free.
    pub(super) fn charge(&self, who: EndpointId, tokens: u64) -> Result<()> {
        let mut u = self.usage.lock_recover();
        let used = u.entry(who).or_default();
        if used.runs >= self.caps.max_runs {
            bail!("steering budget used up: {} runs", self.caps.max_runs);
        }
        if used.tokens.saturating_add(tokens) > self.caps.max_tokens_total {
            bail!(
                "steering budget used up: {} of {} tokens",
                used.tokens,
                self.caps.max_tokens_total
            );
        }
        used.runs += 1;
        used.tokens += tokens;
        Ok(())
    }
}

pub(super) fn load_quarantine(path: &Path) -> std::collections::HashSet<EndpointId> {
    let ids: Vec<String> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    ids.iter().filter_map(|s| s.parse().ok()).collect()
}

impl SteerHost {
    pub(super) fn quarantine(&self, who: EndpointId) {
        let mut q = self.quarantined.lock_recover();
        q.insert(who);
        let ids: Vec<String> = q.iter().map(std::string::ToString::to_string).collect();
        if let Ok(b) = serde_json::to_vec(&ids) {
            let _ = std::fs::write(&self.quarantine_file, b);
        }
    }
}
