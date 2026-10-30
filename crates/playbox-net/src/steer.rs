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

use crate::{
    grants::{Needs, SteerCaps, SteerNeeds},
    wire::{self, Op},
    Client, EndpointId, RoomHandler,
};
use a2a::{Message, Part, SendMessageResponse, Task, TaskState};
use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::SigningKey;
use playbox_core::steer::{Ceiling, Mode, SteerReceipt, SteerSpec};
use playbox_runner::{Mount, TaskSpec};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Largest direction file (a 262144-wide model) and prompt set the host will take.
pub const MAX_DIRECTION_BYTES: usize = 1 << 20;
pub const MAX_PROMPT_BYTES: usize = 512 << 10;
/// Largest handoff envelope (record, text and KV cache) the host will take.
pub const MAX_HANDOFF_BYTES: usize = 128 << 20;
/// Default chance that the host recomputes a handed-over cache from its text.
pub const DEFAULT_AUDIT_PROB: f64 = 0.1;
/// Largest engine output the host will read back.
const MAX_OUTPUT_BYTES: u64 = 8 << 20;

/// Container paths the engine sees. Fixed, so the command line carries nothing a member chose.
const WEIGHTS_AT: &str = "/weights";
const GUARDS_AT: &str = "/guards";

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
struct Usage {
    runs: u64,
    tokens: u64,
}

/// A host's steering configuration: which weights it serves, the engine image, the limits, and what it
/// issues to members.
pub struct SteerHost {
    image: String,
    limits: SteerLimits,
    caps: SteerCaps,
    ceiling: Ceiling,
    /// Weights manifest hash (hex) -> directory.
    weights: BTreeMap<String, PathBuf>,
    /// Weights manifest hash -> (guards directory, classes it defines).
    guards: HashMap<String, (PathBuf, Vec<String>)>,
    usage: Mutex<HashMap<EndpointId, Usage>>,
    /// Chance, per handoff, of recomputing the cache. The engine draws it from the OS random source.
    audit_prob: f64,
    /// Senders caught handing over a cache that was not computed from its text.
    quarantined: Mutex<std::collections::HashSet<EndpointId>>,
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
    fn charge(&self, who: EndpointId, tokens: u64) -> Result<()> {
        let mut u = self.usage.lock().expect("usage lock");
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

fn load_quarantine(path: &Path) -> std::collections::HashSet<EndpointId> {
    let ids: Vec<String> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    ids.iter().filter_map(|s| s.parse().ok()).collect()
}

impl SteerHost {
    fn quarantine(&self, who: EndpointId) {
        let mut q = self.quarantined.lock().expect("quarantine lock");
        q.insert(who);
        let ids: Vec<String> = q.iter().map(std::string::ToString::to_string).collect();
        if let Ok(b) = serde_json::to_vec(&ids) {
            let _ = std::fs::write(&self.quarantine_file, b);
        }
    }
}

/// The text a host signs to say what it decided about one handoff.
fn decision_body(host: &[u8], signer: &[u8], weights: &str, envelope: &[u8], accepted: bool, audited: bool) -> String {
    format!(
        "playbox.handoff-decision.v1\nhost {}\nsigner {}\nweights {weights}\nenvelope {}\naccepted {accepted}\naudited {audited}\n",
        hex(host),
        hex(signer),
        hex(blake3::hash(envelope).as_bytes()),
    )
}

/// Check a host's signed statement that it decided this about exactly these handoff bytes.
/// Returns whether the host accepted.
pub fn verify_decision(text: &str, host: &[u8; 32], signer: &[u8; 32], weights: &str, envelope: &[u8]) -> Result<bool> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let (body, sig) = text.rsplit_once("sig ").ok_or_else(|| anyhow!("the decision has no signature"))?;
    let sig = hex_to_64(sig.trim())?;
    VerifyingKey::from_bytes(host)
        .map_err(|e| anyhow!("{e}"))?
        .verify(body.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| anyhow!("the decision is not signed by the host"))?;
    let accepted = body.contains("\naccepted true\n");
    let audited = body.contains("\naudited true\n");
    if body != decision_body(host, signer, weights, envelope, accepted, audited) {
        bail!("the decision is about a different handoff");
    }
    Ok(accepted)
}

fn hex_to_64(s: &str) -> Result<[u8; 64]> {
    if s.len() != 128 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("bad signature encoding");
    }
    let mut out = [0u8; 64];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)?;
    }
    Ok(out)
}

#[must_use]
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex32(s: &str) -> Result<[u8; 32]> {
    playbox_core::record::unhex::<32>(s).map_err(|e| anyhow!("{e}"))
}

fn hash_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

fn collect(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let ty = e.file_type()?; // does not follow symlinks
        let r = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        if ty.is_dir() {
            collect(&e.path(), &r, out)?;
        } else if ty.is_file() {
            out.push((r, e.path()));
        }
    }
    Ok(())
}

/// Hash of a weights directory: blake3 of `relpath<space>blake3(file)<newline>` lines sorted by the UTF-8
/// bytes of the path, over regular files only, skipping dot-files and symlinks. The Python engine computes
/// the same value (`playbox_steer manifest`).
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn weights_manifest(dir: &Path) -> Result<[u8; 32]> {
    let mut files = Vec::new();
    collect(dir, "", &mut files)?;
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut text = String::new();
    for (rel, p) in files {
        text.push_str(&format!("{rel} {}\n", hash_file(&p)?));
    }
    Ok(*blake3::hash(text.as_bytes()).as_bytes())
}

/// The prompt set as the host reads it, mirroring the engine's checks so a bad one is refused before a
/// container starts.
#[derive(Debug, PartialEq, Eq)]
pub struct PromptSet {
    pub prompts: usize,
    pub max_new_tokens: u64,
}

impl PromptSet {
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let v: Value = serde_json::from_slice(bytes).context("prompt set is not JSON")?;
        let obj = v
            .as_object()
            .ok_or_else(|| anyhow!("prompt set must be an object"))?;
        if let Some(k) = obj
            .keys()
            .find(|k| !matches!(k.as_str(), "prompts" | "max_new_tokens" | "temperature"))
        {
            bail!("unexpected prompt set field {k:?}");
        }
        let list = obj
            .get("prompts")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("prompt set needs a prompts list"))?;
        if list.is_empty() || list.len() > 256 {
            bail!("need 1 to 256 prompts");
        }
        for p in list {
            match p.as_str() {
                Some(s) if !s.is_empty() && s.chars().count() <= 8192 => {}
                _ => bail!("each prompt must be a non-empty string under 8192 characters"),
            }
        }
        let n = match obj.get("max_new_tokens") {
            None => 32,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=512).contains(n))
                .ok_or_else(|| anyhow!("max_new_tokens must be 1..512"))?,
        };
        if let Some(t) = obj.get("temperature") {
            if !t.as_f64().is_some_and(|t| (0.0..=2.0).contains(&t)) {
                bail!("temperature must be 0..2");
            }
        }
        Ok(Self {
            prompts: list.len(),
            max_new_tokens: n,
        })
    }

    /// Tokens a run generates: every prompt twice (steered and clean).
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.prompts as u64 * self.max_new_tokens * 2
    }
}

/// A scratch directory removed on drop, even if the run is cancelled.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let r = crate::random_bytes()?;
        let dir = std::env::temp_dir().join(format!("playbox-steer-{}", hex(&r[..8])));
        std::fs::create_dir(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_output(path: &Path) -> Result<Vec<u8>> {
    // The task wrote here, so treat the path as untrusted: no symlinks, regular file, bounded.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x0100 /* O_NOFOLLOW on macOS and Linux */)
        .open(path)
        .context("the engine wrote no output")?;
    let meta = f.metadata()?;
    if !meta.is_file() || meta.len() > MAX_OUTPUT_BYTES {
        bail!(
            "engine output is not a regular file under {} MiB",
            MAX_OUTPUT_BYTES >> 20
        );
    }
    let mut buf = Vec::new();
    f.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf)?;
    let v: Value = serde_json::from_slice(&buf).context("engine output is not JSON")?;
    if !v.is_object() {
        bail!("engine output is not a JSON object");
    }
    Ok(buf)
}

impl RoomHandler {
    /// Handle a `steer` or `steer_probe` message. The route has already checked that the grant has the
    /// right; this authorizes the specifics and runs.
    pub(crate) async fn steer_message(
        &self,
        from: EndpointId,
        grant: Option<&str>,
        op: Op,
        msg: &Message,
    ) -> Result<Value, a2a::JsonRpcError> {
        let Op::Steer { spec: text, probe } = op else {
            unreachable!("steer_message is only called for Op::Steer")
        };
        let t0 = Instant::now();
        let prepared = match self.steer_prepare(from, grant, &text, probe, msg) {
            Ok(p) => p,
            Err(Prep::Unauthorized(e)) => {
                self.audit.record(
                    "steer_denied",
                    Some(&from.to_string()),
                    json!({ "reason": crate::audit::clip(&format!("{e:#}")) }),
                );
                return Err(crate::unauthorized(e));
            }
            Err(Prep::Refused(e)) => {
                let task = crate::finish(Err(e));
                self.remember(from, &task);
                return serde_json::to_value(SendMessageResponse::Task(task))
                    .map_err(|e| a2a::A2AError::internal(e.to_string()).to_jsonrpc_error());
            }
        };
        let details = json!({
            "kind": if probe { "probe" } else { "run" },
            "model": crate::audit::clip(&prepared.spec.model),
            "weights": hex(&prepared.spec.weights)[..16].to_string(),
            "spec": hex(&prepared.spec.digest().unwrap_or([0; 32]))[..16].to_string(),
            "tokens": prepared.tokens,
        });
        let r = self.steer_run(from, prepared).await;
        self.audit_op(from, "steer", &r, t0, details);
        let task = crate::finish(r);
        self.remember(from, &task);
        serde_json::to_value(SendMessageResponse::Task(task))
            .map_err(|e| a2a::A2AError::internal(e.to_string()).to_jsonrpc_error())
    }

    /// Handle a `handoff` message: check a signed KV-cache record from `from` against a hosted model, with a
    /// random recompute audit. Needs the `handoff` right (issued with steering) and a hosted model.
    pub(crate) async fn handoff_message(
        &self,
        from: EndpointId,
        op: Op,
        msg: &Message,
    ) -> Result<Value, a2a::JsonRpcError> {
        let Op::Handoff { weights } = op else {
            unreachable!("handoff_message is only called for Op::Handoff")
        };
        let t0 = Instant::now();
        let r = self.handoff_run(from, &weights, msg).await;
        let details = json!({ "weights": crate::audit::clip(&weights) });
        self.audit_op(from, "handoff", &r, t0, details);
        let task = crate::finish(r);
        self.remember(from, &task);
        serde_json::to_value(SendMessageResponse::Task(task))
            .map_err(|e| a2a::A2AError::internal(e.to_string()).to_jsonrpc_error())
    }

    async fn handoff_run(&self, from: EndpointId, weights: &str, msg: &Message) -> Result<Task> {
        let host = self
            .cfg
            .steer
            .clone()
            .ok_or_else(|| crate::refuse("this room does not host steering"))?;
        let runner = self
            .cfg
            .runner
            .clone()
            .ok_or_else(|| crate::refuse("this room runs no tasks"))?;
        if host
            .quarantined
            .lock()
            .expect("quarantine lock")
            .contains(&from)
        {
            return Err(crate::refuse(
                "you are quarantined: an earlier handoff of yours failed the audit",
            ));
        }
        let weights_dir = host
            .weights
            .get(weights)
            .ok_or_else(|| crate::refuse(format!("this host does not serve model {weights}")))?
            .clone();
        let parts = wire::raw_parts(msg);
        let [data] = parts.as_slice() else {
            return Err(crate::refuse(
                "a handoff message needs exactly one raw part",
            ));
        };
        if data.is_empty() || data.len() > MAX_HANDOFF_BYTES {
            return Err(crate::refuse("the handoff is empty or too large"));
        }
        let _permit = self.tasks.acquire().await?;
        let scratch = Scratch::new()?;
        let dir = &scratch.0;
        std::fs::write(dir.join("handoff.bin"), data)?;
        std::fs::write(dir.join("signer.txt"), hex(from.as_bytes()))?;
        let mounts = vec![Mount {
            host: std::fs::canonicalize(&weights_dir)?,
            container: WEIGHTS_AT.into(),
        }];
        let mut task = TaskSpec::new(&[]);
        task.cmd = [
            "python",
            "-m",
            "playbox_steer",
            "handoff",
            "--weights",
            WEIGHTS_AT,
            "--task",
            "/workspace",
            "--expected",
            weights,
            "--audit",
            &host.audit_prob.to_string(),
            "--out",
            "/workspace/out.json",
        ]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
        task.image = host.image.clone();
        task.cpus = host.limits.cpus;
        task.mem_mib = host.limits.mem_mib;
        task.timeout_secs = host.limits.timeout_secs;
        let output = tokio::select! {
            r = runner.run_with_mounts(&task, dir, &mounts) => r?,
            why = playbox_runner::quota_exceeded(dir, self.cfg.scratch_quota) => return Err(why),
            () = self.until_banned(from) => return Err(crate::refuse("cancelled: your access to this room was revoked")),
        };
        let out = read_output(&dir.join("out.json"));
        match (output.exit_code, out) {
            (Some(code @ (0 | 5)), Ok(bytes)) => {
                if code == 5
                    && serde_json::from_slice::<Value>(&bytes)?["quarantine"] == json!(true)
                {
                    host.quarantine(from);
                }
                let state = if code == 0 {
                    TaskState::Completed
                } else {
                    TaskState::Failed
                };
                let v: Value = serde_json::from_slice(&bytes)?;
                let body = decision_body(
                    self.attest_key.verifying_key().as_bytes(),
                    from.as_bytes(),
                    weights,
                    data,
                    code == 0,
                    v["audited"] == json!(true),
                );
                let sig = ed25519_dalek::Signer::sign(&*self.attest_key, body.as_bytes());
                let receipt = format!("{body}sig {}", hex(&sig.to_bytes()));
                Ok(wire::task(
                    state,
                    (code == 5).then(|| "the handoff was rejected".to_string()),
                    vec![
                        wire::text_artifact(
                            "decision",
                            String::from_utf8_lossy(&bytes).into_owned(),
                        ),
                        wire::text_artifact("receipt", receipt),
                    ],
                    vec![("kind", json!("handoff"))],
                ))
            }
            (code, _) => Err(anyhow!(
                "handoff check failed (exit {code:?}): {}",
                output
                    .stderr
                    .lines()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" | ")
            )),
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is bounded, or truncated on purpose"
    )]
    fn steer_prepare(
        &self,
        from: EndpointId,
        grant: Option<&str>,
        text: &str,
        probe: bool,
        msg: &Message,
    ) -> Result<Prepared, Prep> {
        let host = self
            .cfg
            .steer
            .clone()
            .ok_or_else(|| Prep::Refused(crate::refuse("this room does not host steering")))?;
        let refuse = |m: String| Prep::Refused(crate::refuse(m));
        let (spec, signer) = SteerSpec::open(text).map_err(|e| refuse(format!("bad spec: {e}")))?;
        if &signer != from.as_bytes() {
            return Err(refuse(
                "the spec must be signed by the key that sends it".into(),
            ));
        }
        let parts = wire::raw_parts(msg);
        let [direction, prompts] = parts.as_slice() else {
            return Err(refuse(
                "a steer message needs two raw parts: the direction file, then the prompt set"
                    .into(),
            ));
        };
        if direction.is_empty() || direction.len() > MAX_DIRECTION_BYTES || direction.len() % 4 != 0
        {
            return Err(refuse(
                "the direction must be a non-empty float32 array under 1 MiB".into(),
            ));
        }
        if prompts.len() > MAX_PROMPT_BYTES {
            return Err(refuse("the prompt set is too large".into()));
        }
        if blake3::hash(direction).as_bytes() != &spec.direction {
            return Err(refuse(
                "the direction file does not hash to the spec's direction".into(),
            ));
        }
        if blake3::hash(prompts).as_bytes() != &spec.prompts {
            return Err(refuse(
                "the prompt set does not hash to the spec's prompts".into(),
            ));
        }
        let ps = PromptSet::parse(prompts).map_err(|e| refuse(format!("{e:#}")))?;
        let weights_hex = hex(&spec.weights);
        let weights_dir = host
            .weights
            .get(&weights_hex)
            .ok_or_else(|| refuse(format!("this host does not serve model {weights_hex}")))?
            .clone();

        let lo = i64::from(*spec.layers.first().expect("validated non-empty"));
        let hi = i64::from(*spec.layers.last().expect("validated non-empty"));
        let op = if probe { "steer_probe" } else { "steer" };
        let tokens = if probe { 0 } else { ps.tokens() };
        let l = &host.limits;
        let needs = Needs {
            cpu_milli: (l.cpus * 1000.0).ceil() as i64,
            mem_mib: i64::from(l.mem_mib),
            timeout_secs: i64::try_from(l.timeout_secs).unwrap_or(i64::MAX),
            steer: Some(SteerNeeds {
                model: weights_hex.clone(),
                layer_lo: lo,
                layer_hi: hi,
                coef_abs_milli: spec.coef_milli.abs(),
                mode: mode_name(spec.mode).into(),
                direction: hex(&spec.direction),
                tokens: i64::try_from(tokens).unwrap_or(i64::MAX),
            }),
            ..Needs::default()
        };
        self.authorize(grant, from, op, &needs)
            .map_err(|e| Prep::Unauthorized(anyhow!("{}", e.message)))?;

        // Which guard classes this member may not steer along: every class the host defines that the grant
        // does not name.
        let mut deny = Vec::new();
        if let Some((_, classes)) = host.guards.get(&weights_hex) {
            for c in classes {
                let q = Needs {
                    class: Some(c.clone()),
                    ..Needs::default()
                };
                if self.authorize(grant, from, "steer_class", &q).is_err() {
                    deny.push(c.clone());
                }
            }
        }
        host.charge(from, tokens)
            .map_err(|e| refuse(format!("{e:#}")))?;
        Ok(Prepared {
            host,
            spec,
            probe,
            direction: direction.to_vec(),
            prompts: prompts.to_vec(),
            weights_dir,
            deny,
            tokens,
        })
    }

    async fn steer_run(&self, from: EndpointId, p: Prepared) -> Result<Task> {
        let runner = self
            .cfg
            .runner
            .clone()
            .ok_or_else(|| crate::refuse("this room runs no tasks"))?;
        let Prepared {
            host,
            spec,
            probe,
            direction,
            prompts,
            weights_dir,
            deny,
            tokens: _,
        } = p;
        let _permit = self.tasks.acquire().await?;
        let scratch = Scratch::new()?;
        let dir = &scratch.0;
        std::fs::write(
            dir.join("spec.txt"),
            spec.canonical_body().map_err(|e| anyhow!(e))?,
        )?;
        std::fs::write(dir.join("direction.f32"), &direction)?;
        std::fs::write(dir.join("prompts.json"), &prompts)?;

        let mut mounts = vec![Mount {
            host: std::fs::canonicalize(&weights_dir)?,
            container: WEIGHTS_AT.into(),
        }];
        let mut cmd: Vec<String> = [
            "python",
            "-m",
            "playbox_steer",
            if probe { "probe" } else { "run" },
            "--weights",
            WEIGHTS_AT,
            "--task",
            "/workspace",
            "--out",
            "/workspace/out.json",
        ]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
        if !probe {
            if let Some((gdir, _)) = host.guards.get(&hex(&spec.weights)) {
                mounts.push(Mount {
                    host: std::fs::canonicalize(gdir)?,
                    container: GUARDS_AT.into(),
                });
                cmd.extend([
                    "--guards".into(),
                    GUARDS_AT.into(),
                    "--deny".into(),
                    deny.join(","),
                ]);
            }
        }
        let mut task = TaskSpec::new(&[]);
        task.cmd = cmd;
        task.image = host.image.clone();
        task.cpus = host.limits.cpus;
        task.mem_mib = host.limits.mem_mib;
        task.timeout_secs = host.limits.timeout_secs;

        let image = runner
            .image_digest(&host.image)
            .await
            .map_err(|e| crate::refuse(format!("cannot identify the engine image: {e:#}")))?;
        let output = tokio::select! {
            r = runner.run_with_mounts(&task, dir, &mounts) => r?,
            why = playbox_runner::quota_exceeded(dir, self.cfg.scratch_quota) => return Err(why),
            () = self.until_banned(from) => return Err(crate::refuse("cancelled: your access to this room was revoked")),
        };

        let kind = if probe { "probe" } else { "run" };
        let out_json = read_output(&dir.join("out.json"));
        let meta = |extra: Vec<(&'static str, Value)>| {
            let mut m = vec![
                ("runner", json!(runner.name())),
                ("specDigest", json!(hex(&spec.digest().unwrap_or([0; 32])))),
                ("kind", json!(kind)),
            ];
            if let Some(c) = output.exit_code {
                m.push(("exitCode", json!(c)));
            }
            m.extend(extra);
            m
        };
        let tail = |s: &str| {
            s.lines()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        };
        match (output.exit_code, out_json) {
            (Some(0), Ok(bytes)) => {
                let request = format!(
                    "{}kind {kind}\n",
                    spec.canonical_body().map_err(|e| anyhow!(e))?
                );
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
                    .unwrap_or(0);
                let receipt = SteerReceipt::sign(
                    &spec,
                    image,
                    request.as_bytes(),
                    &bytes,
                    host.ceiling,
                    now,
                    &self.attest_key,
                )
                .map_err(|e| anyhow!(e))?;
                let m = meta(vec![
                    ("ceiling", json!(ceiling_name(host.ceiling))),
                    ("imageDigest", json!(hex(&image))),
                ]);
                Ok(wire::task(
                    TaskState::Completed,
                    None,
                    vec![
                        wire::text_artifact("output", String::from_utf8_lossy(&bytes).into_owned()),
                        wire::text_artifact("receipt", receipt),
                    ],
                    m,
                ))
            }
            // A failed gate still has an output that says which one.
            (Some(4), Ok(bytes)) => Ok(wire::task(
                TaskState::Failed,
                Some("a pre-flight gate failed; no steered text was produced".into()),
                vec![wire::text_artifact(
                    "output",
                    String::from_utf8_lossy(&bytes).into_owned(),
                )],
                meta(vec![]),
            )),
            (Some(3), _) => Err(crate::refuse(format!(
                "refused by a safety guard: {}",
                tail(&output.stderr)
            ))),
            (Some(2), _) => Err(crate::refuse(format!(
                "the engine rejected the request: {}",
                tail(&output.stderr)
            ))),
            (code, _) if output.timed_out => Err(anyhow!(
                "steering timed out after {}s (exit {code:?})",
                host.limits.timeout_secs
            )),
            (code, out) => Err(anyhow!(
                "steering failed (exit {code:?}, output {}): {}",
                if out.is_ok() { "present" } else { "missing" },
                tail(&output.stderr)
            )),
        }
    }

    /// Resolves once `who` is banned, here or by a notice that reached this host.
    async fn until_banned(&self, who: EndpointId) {
        loop {
            if self.state.lock().expect("state lock").banned.contains(&who) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

enum Prep {
    Unauthorized(anyhow::Error),
    Refused(anyhow::Error),
}

struct Prepared {
    host: std::sync::Arc<SteerHost>,
    spec: SteerSpec,
    probe: bool,
    direction: Vec<u8>,
    prompts: Vec<u8>,
    weights_dir: PathBuf,
    deny: Vec<String>,
    tokens: u64,
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Add => "add",
        Mode::Clamp => "clamp",
        Mode::Ablate => "ablate",
    }
}

fn ceiling_name(c: Ceiling) -> &'static str {
    match c {
        Ceiling::HostClaim => "host_claim",
        Ceiling::LocalInternalExploratory => "local_internal_exploratory",
        Ceiling::Attested => "attested",
    }
}

/// What a client gets back from a completed steer run.
#[derive(Debug, Clone)]
pub struct SteerResult {
    /// The engine's JSON output, exactly the bytes the receipt covers.
    pub output: String,
    /// The host's receipt, already checked against the spec, the output and the host's key.
    pub receipt: SteerReceipt,
    /// The receipt record exactly as the host signed it: what an evidence record's digest covers.
    pub receipt_text: String,
    pub task_id: String,
}

impl Client {
    /// Hand the host a signed handoff envelope (`PBH1`) for the hosted model `weights` (hex manifest hash).
    /// Returns the host's decision JSON; `Err` if the host refused or the cache was rejected.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn handoff(&self, weights: &str, envelope: &[u8]) -> Result<Value> {
        let op = Op::Handoff {
            weights: weights.to_string(),
        };
        let t = self
            .send_task("handoff", &op, vec![Part::raw(envelope.to_vec())])
            .await?;
        let d = wire::artifact_text(&t, "decision")
            .ok_or_else(|| anyhow!("the host sent no decision"))?;
        let receipt = wire::artifact_text(&t, "receipt")
            .ok_or_else(|| anyhow!("the host sent no signed decision"))?;
        let me = self.endpoint.id();
        let accepted = verify_decision(&receipt, self.host_id().as_bytes(), me.as_bytes(), weights, envelope)?;
        let v: Value = serde_json::from_str(&d)?;
        if !accepted || v["accepted"] != json!(true) {
            bail!("handoff rejected: {}", v["reasons"]);
        }
        Ok(v)
    }

    /// Sign `spec` with this client's key and run it on the host. `direction` is the unit-norm float32
    /// file and `prompts` the prompt-set JSON; both must hash to what `spec` names.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn steer(
        &self,
        spec: &SteerSpec,
        direction: &[u8],
        prompts: &[u8],
    ) -> Result<SteerResult> {
        self.steer_op(spec, direction, prompts, false).await
    }

    /// Read-only: project the prompts' activations onto the direction.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn steer_probe(
        &self,
        spec: &SteerSpec,
        direction: &[u8],
        prompts: &[u8],
    ) -> Result<SteerResult> {
        self.steer_op(spec, direction, prompts, true).await
    }

    async fn steer_op(
        &self,
        spec: &SteerSpec,
        direction: &[u8],
        prompts: &[u8],
        probe: bool,
    ) -> Result<SteerResult> {
        let key = SigningKey::from_bytes(&self.endpoint.secret_key().to_bytes());
        let text = spec.sign(&key).map_err(|e| anyhow!(e))?;
        let op = Op::Steer { spec: text, probe };
        let extra = vec![Part::raw(direction.to_vec()), Part::raw(prompts.to_vec())];
        let t = self.send_task("steer", &op, extra).await?;
        let output =
            wire::artifact_text(&t, "output").ok_or_else(|| anyhow!("the host sent no output"))?;
        let receipt_text = wire::artifact_text(&t, "receipt")
            .ok_or_else(|| anyhow!("the host sent no receipt"))?;
        let receipt = SteerReceipt::open(&receipt_text).map_err(|e| anyhow!("bad receipt: {e}"))?;
        if &receipt.host != self.host_id().as_bytes() {
            bail!("the receipt is signed by a key other than the host this client is connected to");
        }
        receipt
            .matches(spec, output.as_bytes())
            .map_err(|e| anyhow!("{e}"))?;
        let _ = unhex32; // kept for callers that decode hashes from task metadata
        Ok(SteerResult {
            output,
            receipt,
            receipt_text,
            task_id: t.id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_sets_are_checked_like_the_engine_checks_them() {
        let ok = |s: &str| PromptSet::parse(s.as_bytes());
        assert_eq!(
            ok(r#"{"prompts":["a","b"],"max_new_tokens":10}"#).unwrap(),
            PromptSet {
                prompts: 2,
                max_new_tokens: 10
            }
        );
        assert_eq!(ok(r#"{"prompts":["a"]}"#).unwrap().max_new_tokens, 32);
        assert_eq!(
            ok(r#"{"prompts":["a","b"],"max_new_tokens":10}"#)
                .unwrap()
                .tokens(),
            40
        );
        for bad in [
            "[]",
            "{}",
            r#"{"prompts":[]}"#,
            r#"{"prompts":[""]}"#,
            r#"{"prompts":[1]}"#,
            r#"{"prompts":["a"],"max_new_tokens":0}"#,
            r#"{"prompts":["a"],"max_new_tokens":513}"#,
            r#"{"prompts":["a"],"max_new_tokens":1.5}"#,
            r#"{"prompts":["a"],"temperature":2.5}"#,
            r#"{"prompts":["a"],"temperature":"hot"}"#,
            r#"{"prompts":["a"],"extra":1}"#,
            "not json",
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
        let many = format!(r#"{{"prompts":[{}]}}"#, vec!["\"a\""; 257].join(","));
        assert!(ok(&many).is_err());
    }

    #[test]
    fn manifest_matches_the_python_engine() {
        // The expected value is `python -m playbox_steer manifest` over the same tree (see
        // steer/tests/test_engine.py::test_manifest_cross_check_fixture).
        let dir = std::env::temp_dir().join(format!("pb-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha").unwrap();
        std::fs::write(dir.join("a-b.txt"), "dash").unwrap();
        std::fs::write(dir.join("sub").join("c.bin"), [0u8, 1, 2, 3]).unwrap();
        std::fs::create_dir_all(dir.join(".cache")).unwrap();
        std::fs::write(dir.join(".cache").join("ignored"), "x").unwrap();
        std::os::unix::fs::symlink(dir.join("a.txt"), dir.join("link")).unwrap();
        assert_eq!(hex(&weights_manifest(&dir).unwrap()), MANIFEST_FIXTURE);
        std::fs::write(dir.join("a.txt"), "alphA").unwrap();
        assert_ne!(hex(&weights_manifest(&dir).unwrap()), MANIFEST_FIXTURE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Filled in from the Python implementation; see test_manifest_cross_check_fixture.
    const MANIFEST_FIXTURE: &str =
        "7bc07aaa2a32a98a50e6abae371e4e48a068edd147265ea594bee1c8f13ec519";

    #[test]
    fn scratch_dirs_are_removed_on_drop() {
        let p = {
            let s = Scratch::new().unwrap();
            std::fs::write(s.0.join("x"), "1").unwrap();
            s.0.clone()
        };
        assert!(!p.exists());
    }

    #[test]
    fn output_reader_refuses_symlinks_directories_and_non_objects() {
        let dir = Scratch::new().unwrap();
        let f = dir.0.join("out.json");
        std::fs::write(&f, r#"{"a":1}"#).unwrap();
        assert!(read_output(&f).is_ok());
        std::fs::write(&f, "[1]").unwrap();
        assert!(read_output(&f).is_err());
        std::fs::write(&f, "garbage").unwrap();
        assert!(read_output(&f).is_err());
        let target = dir.0.join("secret");
        std::fs::write(&target, r#"{"secret":true}"#).unwrap();
        let link = dir.0.join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_output(&link).is_err(), "symlink must not be followed");
        assert!(read_output(&dir.0.join("missing.json")).is_err());
    }
}
