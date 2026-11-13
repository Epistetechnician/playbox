//! Capabilities: named operations the host owner offers to members, each run in the sandbox, once, with bounded arguments.
//!
//! The idea is Peerspan's shared capabilities (a peer calls what an owner chose to expose; the owner may approve each call), built
//! on what Playbox already has:
//!
//! * **Authority.** A member may call a capability only if its grant lists `cap/NAME`, the same allow-list that holds hosts and
//!   secrets, so a member can narrow it offline and cannot widen it.
//! * **Bounds.** Each parameter is declared; a value outside its allowed list, over its length, with a control character, or (for an
//!   unlisted parameter) starting with `-` is refused when the call is submitted. Arguments become separate argv entries, never a
//!   shell string, so a value cannot add words or flags.
//! * **Sandbox.** The command runs through the host's runner (microVM or container), in an empty scratch directory, with the
//!   capability's own image, memory and time. It does not run with the owner's authority.
//! * **Exactly once.** A call is written to disk before it runs. The same `(member, call_id)` with the same request returns the
//!   existing call; with a different request it is refused. After a restart a call that was running is `abandoned`, never run again.
//! * **Approval.** A capability may need the owner's go-ahead per call: the call waits as `awaiting_approval` until the owner approves
//!   or denies it (console `approve ID` / `deny ID`). Unanswered calls expire.
//! * **Verifiable result.** The result is hashed over a canonical encoding (keys sorted, no whitespace) and the hash goes into the
//!   audit log with the call id, so the output a member received can be checked against the log.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub const MAX_ARGS: usize = 16;
pub const DEFAULT_MAX_LEN: usize = 256;
/// Calls kept per member; finished ones are dropped oldest first.
pub const MAX_CALLS_PER_MEMBER: usize = 256;
/// Calls a member may have waiting or running at once.
pub const MAX_OPEN_PER_MEMBER: usize = 8;
/// An unanswered approval request lapses after this long.
pub const APPROVAL_TTL_MS: u64 = 15 * 60 * 1000;
const MAX_OUTPUT: usize = 64 * 1024;

/// Bounds on one parameter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParamRule {
    /// If set, the value must be one of these exactly.
    #[serde(default)]
    pub allowed: Option<Vec<String>>,
    #[serde(default)]
    pub max_len: Option<usize>,
}

/// One operation the owner exposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    /// The command. An entry may contain `{param}` placeholders, filled from the call's arguments.
    pub argv: Vec<String>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamRule>,
    /// Each call waits for the owner's approval.
    #[serde(default)]
    pub approval: bool,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub mem_mib: Option<u32>,
}

fn plain_name(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

impl Capability {
    /// Check a definition: names, placeholders that match declared parameters, and no placeholder in the program position.
    ///
    /// # Errors
    ///
    /// Describes the first problem found.
    pub fn validate(&self) -> Result<()> {
        if !plain_name(&self.name, 48) {
            bail!("capability name {:?} must be 1-48 letters, digits, - _ .", self.name);
        }
        if self.argv.is_empty() || self.argv[0].contains('{') {
            bail!("capability {}: the program (first argv entry) must be fixed", self.name);
        }
        for p in self.params.keys() {
            if !plain_name(p, 32) {
                bail!("capability {}: bad parameter name {p:?}", self.name);
            }
        }
        let mut used = std::collections::BTreeSet::new();
        for a in &self.argv {
            let mut rest = a.as_str();
            while let Some(i) = rest.find('{') {
                let Some(j) = rest[i..].find('}') else { bail!("capability {}: unclosed placeholder in {a:?}", self.name) };
                let p = &rest[i + 1..i + j];
                if !self.params.contains_key(p) {
                    bail!("capability {}: placeholder {{{p}}} is not a declared parameter", self.name);
                }
                used.insert(p.to_string());
                rest = &rest[i + j + 1..];
            }
        }
        if let Some(unused) = self.params.keys().find(|p| !used.contains(*p)) {
            bail!("capability {}: parameter {unused} is declared but never used", self.name);
        }
        for (p, r) in &self.params {
            if let Some(list) = &r.allowed {
                if list.is_empty() {
                    bail!("capability {}: parameter {p} has an empty allowed list", self.name);
                }
            }
        }
        Ok(())
    }

    /// Check the arguments of a call against the bounds and build the command.
    ///
    /// # Errors
    ///
    /// Names the argument that is missing, unknown, too long, not allowed or unsafe.
    pub fn bind(&self, args: &BTreeMap<String, String>) -> Result<Vec<String>> {
        if args.len() > MAX_ARGS {
            bail!("too many arguments");
        }
        for k in args.keys() {
            if !self.params.contains_key(k) {
                bail!("{} takes no argument {k:?}", self.name);
            }
        }
        for (p, rule) in &self.params {
            let v = args.get(p).with_context(|| format!("missing argument {p:?}"))?;
            if v.len() > rule.max_len.unwrap_or(DEFAULT_MAX_LEN) {
                bail!("argument {p:?} is too long");
            }
            if v.chars().any(char::is_control) {
                bail!("argument {p:?} contains a control character");
            }
            match &rule.allowed {
                Some(list) if !list.contains(v) => bail!("argument {p:?} is not one of the values this capability allows"),
                None if v.starts_with('-') => bail!("argument {p:?} may not start with '-' (it could be read as an option)"),
                _ => {}
            }
        }
        // One pass over the template, so text inside a value is never read as another placeholder.
        Ok(self
            .argv
            .iter()
            .map(|a| {
                let mut out = String::new();
                let mut rest = a.as_str();
                while let Some(i) = rest.find('{') {
                    out.push_str(&rest[..i]);
                    let j = rest[i..].find('}').map_or(rest.len(), |j| i + j);
                    out.push_str(args.get(&rest[i + 1..j]).map_or("", String::as_str));
                    rest = rest.get(j + 1..).unwrap_or("");
                }
                out.push_str(rest);
                out
            })
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    AwaitingApproval,
    Running,
    Done,
    Denied,
    Expired,
    /// Was running when the host stopped. It is not run again; the member decides whether to call it again with a new id.
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// blake3 of the canonical result, hex. The audit log holds the same value.
    pub result_hash: String,
    /// The host's ed25519 signature (hex) over the call's identity and `result_hash`, added when the host answers (never stored).
    /// A caller verifies it against the host id it dialed, so a relay or a copy of the result cannot be altered or passed off as
    /// another host's or another member's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_sig: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub id: String,
    pub member: String,
    pub name: String,
    pub args: BTreeMap<String, String>,
    pub request_hash: String,
    pub state: State,
    pub created_ms: u64,
    #[serde(default)]
    pub outcome: Option<Outcome>,
}

const RESULT_DOMAIN: &str = "playbox.cap-result.v1";

impl Call {
    /// The canonical document the result hash covers, rebuilt from the outcome.
    fn result_hash_of(&self, o: &Outcome) -> String {
        let doc = json!({"call_id": self.id, "name": self.name, "args": self.args, "exit_code": o.exit_code, "timed_out": o.timed_out, "stdout": o.stdout, "stderr": o.stderr});
        blake3::hash(canonical(&doc).as_bytes()).to_hex().to_string()
    }

    /// What the host signs: who it is, whose call, which capability and arguments, how it ended, and the result hash.
    fn signed_bytes(&self, host: &[u8; 32], o: &Outcome) -> Vec<u8> {
        let host: String = host.iter().map(|b| format!("{b:02x}")).collect();
        format!("{RESULT_DOMAIN}\n{host}\n{}\n{}\n{}\n{}\n{:?}\n{}", self.member, self.id, self.name, self.request_hash, self.state, o.result_hash).into_bytes()
    }

    /// This call with the host's signature on its result, for sending to the caller.
    #[must_use]
    pub fn signed(&self, key: &ed25519_dalek::SigningKey) -> Self {
        use ed25519_dalek::Signer;
        let mut c = self.clone();
        if let Some(o) = c.outcome.clone() {
            let sig = key.sign(&self.signed_bytes(&key.verifying_key().to_bytes(), &o));
            c.outcome = Some(Outcome { host_sig: Some(sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect()), ..o });
        }
        c
    }

    /// Check a result received from `host`: the hash matches the output, and the host signed it for this member and call.
    ///
    /// # Errors
    ///
    /// Says what does not match. A call without an outcome has nothing to check and passes.
    pub fn verify_result(&self, host: &[u8; 32]) -> Result<()> {
        use ed25519_dalek::Verifier;
        let Some(o) = &self.outcome else { return Ok(()) };
        if self.result_hash_of(o) != o.result_hash {
            bail!("the result of call {} does not match its hash: it was altered", self.id);
        }
        let sig = o.host_sig.as_deref().context("the host did not sign this result (hosts before 0.3.0 do not; ask the host to update)")?;
        let bytes: Vec<u8> = (0..sig.len() / 2).filter_map(|i| u8::from_str_radix(sig.get(2 * i..2 * i + 2)?, 16).ok()).collect();
        let sig: [u8; 64] = bytes.try_into().map_err(|_| anyhow::anyhow!("the host's signature is malformed"))?;
        ed25519_dalek::VerifyingKey::from_bytes(host)?
            .verify(&self.signed_bytes(host, o), &ed25519_dalek::Signature::from_bytes(&sig))
            .map_err(|_| anyhow::anyhow!("the host's signature on call {} does not verify: this result is not the host's", self.id))
    }
}

/// What the caller of [`Book::submit`] or [`Book::decide`] must do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// Start the command now (this is the one time it is told so for this call).
    Run,
    /// A new call that waits for the owner.
    Wait,
    Nothing,
}

#[derive(Default)]
struct Table {
    calls: BTreeMap<String, Call>, // key: "member/id"
}

pub struct Book {
    caps: BTreeMap<String, Capability>,
    table: Mutex<Table>,
    store: Option<PathBuf>,
}

fn key(member: &str, id: &str) -> String {
    format!("{member}/{id}")
}

/// Sorted keys, no whitespace, strings escaped by serde: the same bytes for the same value on any host.
#[must_use]
pub fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys.into_iter().map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&m[k]))).collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Book {
    /// # Errors
    ///
    /// Fails if a definition is invalid, a name repeats, or the store file cannot be read.
    pub fn new(caps: Vec<Capability>, store: Option<PathBuf>) -> Result<Self> {
        let mut map = BTreeMap::new();
        for c in caps {
            c.validate()?;
            if map.insert(c.name.clone(), c).is_some() {
                bail!("two capabilities share a name");
            }
        }
        let mut table = Table::default();
        if let Some(p) = &store {
            if let Ok(text) = std::fs::read_to_string(p) {
                let calls: Vec<Call> = serde_json::from_str(&text).with_context(|| format!("reading {}", p.display()))?;
                for mut c in calls {
                    if c.state == State::Running {
                        c.state = State::Abandoned;
                    }
                    table.calls.insert(key(&c.member, &c.id), c);
                }
            }
        }
        let book = Self { caps: map, table: Mutex::new(table), store };
        book.save(&book.table.lock().expect("calls lock"));
        Ok(book)
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.caps.keys().cloned().collect()
    }

    /// What a member sees of the offer: names, parameters and whether calls need approval. Never the command.
    #[must_use]
    pub fn describe(&self) -> Value {
        json!(self
            .caps
            .values()
            .map(|c| json!({
                "name": c.name,
                "approval": c.approval,
                "params": c.params.iter().map(|(k, r)| json!({"name": k, "allowed": r.allowed, "max_len": r.max_len.unwrap_or(DEFAULT_MAX_LEN)})).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>())
    }

    fn save(&self, t: &Table) {
        if let Some(p) = &self.store {
            let tmp = p.with_extension("tmp");
            let list: Vec<&Call> = t.calls.values().collect();
            if let Ok(text) = serde_json::to_string(&list) {
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, p);
                }
            }
        }
    }

    /// Record a call, or return the one already recorded for this id.
    ///
    /// # Errors
    ///
    /// Refuses an unknown capability, arguments outside the bounds, an id reused with a different request, or a member with too many
    /// open calls.
    pub fn submit(&self, member: &str, name: &str, args: BTreeMap<String, String>, id: &str) -> Result<(Call, Next)> {
        if !plain_name(id, 64) {
            bail!("call_id must be 1-64 letters, digits, - _ .");
        }
        let cap = self.caps.get(name).with_context(|| format!("no capability named {name:?}"))?;
        cap.bind(&args)?;
        let request_hash = blake3::hash(canonical(&json!({"name": name, "args": args})).as_bytes()).to_hex().to_string();
        let mut t = self.table.lock().expect("calls lock");
        self.expire(&mut t);
        let k = key(member, id);
        if let Some(existing) = t.calls.get(&k) {
            if existing.request_hash != request_hash {
                bail!("call_id {id:?} was already used for a different request");
            }
            return Ok((existing.clone(), Next::Nothing));
        }
        let open = t.calls.values().filter(|c| c.member == member && matches!(c.state, State::AwaitingApproval | State::Running)).count();
        if open >= MAX_OPEN_PER_MEMBER {
            bail!("too many open calls ({MAX_OPEN_PER_MEMBER})");
        }
        let mine: Vec<String> = t.calls.iter().filter(|(_, c)| c.member == member).map(|(k, _)| k.clone()).collect();
        if mine.len() >= MAX_CALLS_PER_MEMBER {
            // Drop the oldest finished one.
            if let Some(old) = mine.iter().filter(|k| matches!(t.calls[*k].state, State::Done | State::Denied | State::Expired | State::Abandoned)).min_by_key(|k| t.calls[*k].created_ms).cloned() {
                t.calls.remove(&old);
            } else {
                bail!("too many calls");
            }
        }
        let state = if cap.approval { State::AwaitingApproval } else { State::Running };
        let call = Call { id: id.to_string(), member: member.to_string(), name: name.to_string(), args, request_hash, state, created_ms: now_ms(), outcome: None };
        t.calls.insert(k, call.clone());
        self.save(&t);
        let next = if state == State::Running { Next::Run } else { Next::Wait };
        Ok((call, next))
    }

    fn expire(&self, t: &mut Table) {
        let now = now_ms();
        for c in t.calls.values_mut() {
            if c.state == State::AwaitingApproval && now.saturating_sub(c.created_ms) > APPROVAL_TTL_MS {
                c.state = State::Expired;
            }
        }
    }

    /// A member's own call.
    #[must_use]
    pub fn view(&self, member: &str, id: &str) -> Option<Call> {
        let mut t = self.table.lock().expect("calls lock");
        self.expire(&mut t);
        t.calls.get(&key(member, id)).cloned()
    }

    /// Calls waiting for the owner.
    #[must_use]
    pub fn pending(&self) -> Vec<Call> {
        let mut t = self.table.lock().expect("calls lock");
        self.expire(&mut t);
        t.calls.values().filter(|c| c.state == State::AwaitingApproval).cloned().collect()
    }

    /// The owner's decision on a waiting call, found by `member/id` or, if unambiguous, by id alone.
    ///
    /// # Errors
    ///
    /// Fails if no waiting call matches, or the id matches several.
    pub fn decide(&self, which: &str, approve: bool) -> Result<(Call, Next)> {
        let mut t = self.table.lock().expect("calls lock");
        self.expire(&mut t);
        let hits: Vec<String> = t
            .calls
            .iter()
            .filter(|(k, c)| c.state == State::AwaitingApproval && (k.as_str() == which || c.id == which))
            .map(|(k, _)| k.clone())
            .collect();
        let [k] = hits.as_slice() else { bail!("{} waiting calls match {which:?}", hits.len()) };
        let c = t.calls.get_mut(k).expect("just found");
        c.state = if approve { State::Running } else { State::Denied };
        let out = c.clone();
        self.save(&t);
        let next = if approve { Next::Run } else { Next::Nothing };
        Ok((out, next))
    }

    /// The command and limits for a call that is about to run.
    ///
    /// # Errors
    ///
    /// Fails if the call or its capability no longer exists.
    pub fn plan(&self, member: &str, id: &str) -> Result<(Vec<String>, &Capability)> {
        let t = self.table.lock().expect("calls lock");
        let c = t.calls.get(&key(member, id)).context("call vanished")?;
        let cap = self.caps.get(&c.name).context("capability vanished")?;
        Ok((cap.bind(&c.args)?, cap))
    }

    /// Record how a running call ended. The result hash covers the call id, name, arguments and output.
    pub fn finish(&self, member: &str, id: &str, exit_code: Option<i32>, timed_out: bool, stdout: &str, stderr: &str) -> Option<Call> {
        let mut t = self.table.lock().expect("calls lock");
        let c = t.calls.get_mut(&key(member, id))?;
        if c.state != State::Running {
            return Some(c.clone());
        }
        let clip = |s: &str| s.chars().take(MAX_OUTPUT).collect::<String>();
        let (stdout, stderr) = (clip(stdout), clip(stderr));
        let doc = json!({"call_id": c.id, "name": c.name, "args": c.args, "exit_code": exit_code, "timed_out": timed_out, "stdout": stdout, "stderr": stderr});
        let result_hash = blake3::hash(canonical(&doc).as_bytes()).to_hex().to_string();
        c.outcome = Some(Outcome { exit_code, timed_out, stdout, stderr, result_hash, host_sig: None });
        c.state = State::Done;
        let out = c.clone();
        self.save(&t);
        Some(out)
    }
}

/// Runs calls: the book, the sandbox that executes them, and the log they are recorded in.
pub struct Engine {
    pub book: Book,
    runner: Option<std::sync::Arc<dyn playbox_runner::Runner>>,
    audit: std::sync::Arc<crate::audit::Audit>,
    quota: playbox_runner::Quota,
    limits: playbox_runner::Limits,
    permits: std::sync::Arc<tokio::sync::Semaphore>,
}

impl Engine {
    #[must_use]
    pub fn new(
        book: Book,
        runner: Option<std::sync::Arc<dyn playbox_runner::Runner>>,
        audit: std::sync::Arc<crate::audit::Audit>,
        quota: playbox_runner::Quota,
        limits: playbox_runner::Limits,
    ) -> Self {
        Self { book, runner, audit, quota, limits, permits: std::sync::Arc::new(tokio::sync::Semaphore::new(2)) }
    }

    /// Whether this host can run capabilities at all (it has a sandbox).
    #[must_use]
    pub fn can_run(&self) -> bool {
        self.runner.is_some()
    }

    /// Submit a call, record it in the audit log, and start it if it needs no approval.
    ///
    /// # Errors
    ///
    /// See [`Book::submit`]; also fails when the host has no sandbox to run it in.
    pub fn submit(self: &std::sync::Arc<Self>, member: &str, name: &str, args: BTreeMap<String, String>, id: &str) -> Result<Call> {
        if self.runner.is_none() {
            bail!("this host has no sandbox to run capabilities in");
        }
        let (call, next) = self.book.submit(member, name, args, id)?;
        if next != Next::Nothing {
            self.audit.record("capability_call", Some(member), json!({"call_id": id, "name": name, "request_hash": call.request_hash, "state": call.state}));
        }
        if next == Next::Run {
            self.start(member.to_string(), id.to_string());
        }
        Ok(call)
    }

    /// The owner's decision on a waiting call.
    ///
    /// # Errors
    ///
    /// See [`Book::decide`].
    pub fn decide(self: &std::sync::Arc<Self>, which: &str, approve: bool) -> Result<Call> {
        let (call, next) = self.book.decide(which, approve)?;
        self.audit.record("capability_decision", Some(&call.member), json!({"call_id": call.id, "name": call.name, "approved": approve}));
        if next == Next::Run {
            self.start(call.member.clone(), call.id.clone());
        }
        Ok(call)
    }

    fn start(self: &std::sync::Arc<Self>, member: String, id: String) {
        let me = self.clone();
        tokio::spawn(async move {
            let Some(runner) = me.runner.clone() else { return };
            let _permit = me.permits.acquire().await;
            let (exit, timed_out, out, err) = match me.book.plan(&member, &id) {
                Err(e) => (None, false, String::new(), format!("{e:#}")),
                Ok((argv, cap)) => {
                    let mut spec = playbox_runner::TaskSpec::new(&argv.iter().map(String::as_str).collect::<Vec<_>>());
                    if let Some(i) = &cap.image {
                        spec.image.clone_from(i);
                    }
                    spec.timeout_secs = cap.timeout_secs.unwrap_or(60).min(me.limits.timeout_secs);
                    spec.mem_mib = cap.mem_mib.unwrap_or(512).min(me.limits.mem_mib);
                    let dir = std::env::temp_dir().join(format!("playbox-cap-{}-{}", std::process::id(), blake3::hash(format!("{member}/{id}").as_bytes()).to_hex()));
                    let ran = async {
                        std::fs::create_dir_all(&dir)?;
                        // The task runs as the directory's owner and may be a different user inside a container.
                        #[cfg(unix)]
                        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o777))?;
                        playbox_runner::run_with_quota(runner.as_ref(), &spec, &dir, me.quota).await
                    }
                    .await;
                    let _ = std::fs::remove_dir_all(&dir);
                    match ran {
                        Ok(o) => (o.exit_code, o.timed_out, o.stdout, o.stderr),
                        Err(e) => (None, false, String::new(), format!("{e:#}")),
                    }
                }
            };
            if let Some(call) = me.book.finish(&member, &id, exit, timed_out, &out, &err) {
                if let Some(o) = &call.outcome {
                    me.audit.record("capability_result", Some(&member), json!({"call_id": id, "name": call.name, "result_hash": o.result_hash, "exit_code": o.exit_code, "timed_out": o.timed_out}));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(approval: bool) -> Capability {
        Capability {
            name: "run-tests".into(),
            argv: vec!["cargo".into(), "test".into(), "{filter}".into(), "--features={feat}".into()],
            params: BTreeMap::from([
                ("filter".into(), ParamRule { allowed: None, max_len: Some(16) }),
                ("feat".into(), ParamRule { allowed: Some(vec!["a".into(), "b".into()]), max_len: None }),
            ]),
            approval,
            image: None,
            timeout_secs: None,
            mem_mib: None,
        }
    }

    fn args(f: &str, feat: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("filter".into(), f.into()), ("feat".into(), feat.into())])
    }

    #[test]
    fn arguments_fill_placeholders_and_stay_separate_words() {
        let c = cap(false);
        assert_eq!(c.bind(&args("a b; rm -rf", "a")).unwrap(), ["cargo", "test", "a b; rm -rf", "--features=a"]);
        // A value that looks like a placeholder stays text.
        assert_eq!(c.bind(&args("{feat}", "a")).unwrap()[2], "{feat}");
    }

    #[test]
    fn arguments_outside_the_bounds_are_refused() {
        let c = cap(false);
        for bad in [args("-x", "a"), args("ok", "c"), args(&"x".repeat(17), "a"), args("a\nb", "a"), args("a\u{0}", "a")] {
            assert!(c.bind(&bad).is_err(), "{bad:?}");
        }
        assert!(c.bind(&BTreeMap::from([("filter".into(), "x".into())])).is_err(), "missing");
        let mut extra = args("x", "a");
        extra.insert("other".into(), "1".into());
        assert!(c.bind(&extra).is_err(), "unknown");
    }

    #[test]
    fn bad_definitions_are_refused() {
        let mut c = cap(false);
        c.argv[0] = "{filter}".into();
        assert!(c.validate().is_err());
        let mut c = cap(false);
        c.argv.push("{nope}".into());
        assert!(c.validate().is_err());
        let mut c = cap(false);
        c.params.insert("unused".into(), ParamRule::default());
        assert!(c.validate().is_err());
        let mut c = cap(false);
        c.name = "a b".into();
        assert!(c.validate().is_err());
        assert!(Book::new(vec![cap(false), cap(false)], None).is_err());
    }

    #[test]
    fn a_call_id_runs_once_and_a_retry_returns_the_same_call() {
        let b = Book::new(vec![cap(false)], None).unwrap();
        let (c1, n1) = b.submit("m1", "run-tests", args("x", "a"), "id1").unwrap();
        assert_eq!(n1, Next::Run);
        assert_eq!(c1.state, State::Running);
        let (c2, n2) = b.submit("m1", "run-tests", args("x", "a"), "id1").unwrap();
        assert_eq!(n2, Next::Nothing, "a retry must not start a second run");
        assert_eq!(c2.created_ms, c1.created_ms);
        assert!(b.submit("m1", "run-tests", args("y", "a"), "id1").is_err(), "same id, different request");
        // Another member's id is a different call.
        assert_eq!(b.submit("m2", "run-tests", args("x", "a"), "id1").unwrap().1, Next::Run);
        let done = b.finish("m1", "id1", Some(0), false, "ok", "").unwrap();
        assert_eq!(done.state, State::Done);
        assert_eq!(b.submit("m1", "run-tests", args("x", "a"), "id1").unwrap().1, Next::Nothing, "a finished call is not run again");
    }

    #[test]
    fn approval_gates_the_run_and_unanswered_calls_expire() {
        let b = Book::new(vec![cap(true)], None).unwrap();
        let (c, n) = b.submit("m", "run-tests", args("x", "a"), "c1").unwrap();
        assert_eq!((c.state, n), (State::AwaitingApproval, Next::Wait));
        assert_eq!(b.submit("m", "run-tests", args("x", "a"), "c1").unwrap().1, Next::Nothing, "a retry is not a new request");
        assert_eq!(b.pending().len(), 1);
        let (c, n) = b.decide("c1", true).unwrap();
        assert_eq!((c.state, n), (State::Running, Next::Run));
        assert!(b.decide("c1", true).is_err(), "decided once");
        b.submit("m", "run-tests", args("y", "a"), "c2").unwrap();
        assert_eq!(b.decide("m/c2", false).unwrap().0.state, State::Denied);
        // Expiry.
        b.submit("m", "run-tests", args("z", "a"), "c3").unwrap();
        b.table.lock().unwrap().calls.get_mut("m/c3").unwrap().created_ms = 0;
        assert_eq!(b.view("m", "c3").unwrap().state, State::Expired);
        assert!(b.decide("c3", true).is_err());
    }

    #[test]
    fn a_restart_abandons_running_calls_and_never_reruns_them() {
        let dir = std::env::temp_dir().join(format!("playbox-caps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("calls.json");
        let _ = std::fs::remove_file(&store);
        {
            let b = Book::new(vec![cap(false)], Some(store.clone())).unwrap();
            b.submit("m", "run-tests", args("x", "a"), "r1").unwrap();
            b.submit("m", "run-tests", args("y", "a"), "r2").unwrap();
            b.finish("m", "r2", Some(0), false, "out", "");
        }
        let b = Book::new(vec![cap(false)], Some(store.clone())).unwrap();
        assert_eq!(b.view("m", "r1").unwrap().state, State::Abandoned);
        assert_eq!(b.view("m", "r2").unwrap().state, State::Done);
        assert_eq!(b.submit("m", "run-tests", args("x", "a"), "r1").unwrap().1, Next::Nothing);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_result_is_signed_by_the_host_and_cannot_be_altered_or_moved() {
        use ed25519_dalek::SigningKey;
        let host = SigningKey::from_bytes(&[3u8; 32]);
        let hid = host.verifying_key().to_bytes();
        let b = Book::new(vec![cap(false)], None).unwrap();
        b.submit("alice", "run-tests", args("x", "a"), "s1").unwrap();
        let done = b.finish("alice", "s1", Some(0), false, "hi", "").unwrap();
        let signed = done.signed(&host);
        signed.verify_result(&hid).expect("the host's own result verifies");

        // The output, the member, the call id and the host are all bound.
        let mut t = signed.clone();
        t.outcome.as_mut().unwrap().stdout = "something else".into();
        assert!(t.verify_result(&hid).unwrap_err().to_string().contains("altered"));
        let mut t = signed.clone();
        t.member = "bob".into();
        assert!(t.verify_result(&hid).is_err(), "another member cannot present alice's result");
        let mut t = signed.clone();
        t.id = "s2".into();
        assert!(t.verify_result(&hid).is_err());
        let other = SigningKey::from_bytes(&[4u8; 32]).verifying_key().to_bytes();
        assert!(signed.verify_result(&other).is_err(), "a result from another host does not verify");
        assert!(done.verify_result(&hid).unwrap_err().to_string().contains("did not sign"), "an unsigned result is refused");
    }

    #[test]
    fn the_result_hash_is_canonical_and_covers_the_output() {
        let b = Book::new(vec![cap(false)], None).unwrap();
        b.submit("m", "run-tests", args("x", "a"), "h1").unwrap();
        b.submit("m", "run-tests", args("x", "a"), "h2").unwrap();
        let h1 = b.finish("m", "h1", Some(0), false, "out", "").unwrap().outcome.unwrap().result_hash;
        let h1b = {
            let doc = json!({"stderr": "", "stdout": "out", "timed_out": false, "exit_code": 0, "args": {"feat": "a", "filter": "x"}, "name": "run-tests", "call_id": "h1"});
            blake3::hash(canonical(&doc).as_bytes()).to_hex().to_string()
        };
        assert_eq!(h1, h1b, "key order must not matter");
        let h2 = b.finish("m", "h2", Some(0), false, "different", "").unwrap().outcome.unwrap().result_hash;
        assert_ne!(h1, h2);
        assert_eq!(canonical(&json!({"b": [1, {"d": 1, "c": "x\"y"}], "a": null})), r#"{"a":null,"b":[1,{"c":"x\"y","d":1}]}"#);
    }

    #[test]
    fn open_calls_per_member_are_bounded() {
        let b = Book::new(vec![cap(true)], None).unwrap();
        for i in 0..MAX_OPEN_PER_MEMBER {
            b.submit("m", "run-tests", args("x", "a"), &format!("o{i}")).unwrap();
        }
        assert!(b.submit("m", "run-tests", args("x", "a"), "over").is_err());
        assert!(b.submit("other", "run-tests", args("x", "a"), "o0").is_ok());
    }
}
