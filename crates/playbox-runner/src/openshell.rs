//! The NVIDIA `OpenShell` runner: runs tasks in `OpenShell` sandboxes via the `openshell` CLI
//! (<https://github.com/NVIDIA/OpenShell>).
//!
//! Modes:
//! - Ephemeral (`Runner::run`) and warm stateless (`Runner::run_warm`): every task starts from a clean
//!   checkout of `base` in `/sandbox`; warm sandboxes skip `sandbox create` between commands for the same member.
//! - Stateful session (`Runner::run_session`): keeps `/sandbox` intact across commands for the same
//!   `(authenticated member, base_commit)` so iterative agent loops skip both full-repo re-uploads and
//!   edit-command replay. [`SyncMode::Delta`] mirrors only changed/deleted files back to the host;
//!   [`SyncMode::None`] skips mirroring on read-only steps; [`SyncMode::Full`] mirrors the entire tree.
//! - Host-side `BOX-7` model API egress (`with_egress`): binds a per-task capability-scoped reverse proxy
//!   on the host, opens only that proxy endpoint in `network_policies`, injects capability URLs via an
//!   immediately-unlinked `0600` env file (never in `/proc/cmdline`), and injects credentials host-side.
//!
//! Known gaps, deliberately not papered over:
//! - `OpenShell` policies carry no CPU, memory or timeout limits. The flags are passed, but its VM
//!   driver ignores them, so `preflight` fails closed unless `PLAYBOX_ALLOW_MISSING_LIMITS` is set
//!   (same rule as the Docker runner). Timeouts are enforced here by killing the CLI and deleting
//!   the sandbox.
//! - Network is per host: a task's `allowed_hosts` (plus any host-managed `BOX-7` proxy endpoint) become
//!   `network_policies`; unrestricted network is refused.
//! - Isolation is reported as `Container`, the weakest claim that holds across its drivers.
//!   Tested against `OpenShell` 0.1.6 with the local VM driver (`tests/openshell.rs`, needs a gateway).

use super::{
    check_spec, egress, parse_image_id, run_child, secrets::SecretStore, Isolation, Runner,
    SandboxPolicy, SyncMode, TaskOutput, TaskSpec,
};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use std::{
    collections::{BTreeMap, HashMap},
    net::Ipv4Addr,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

const BIN: &str = "openshell";
const SETUP_TIMEOUT: Duration = Duration::from_secs(180);

pub struct OpenShellRunner {
    policy: SandboxPolicy,
    /// `openshell --version`, read by `preflight`.
    version: std::sync::OnceLock<String>,
    /// One warm sandbox per session key (see [`Runner::run_warm`] and [`Runner::run_session`]).
    warm: tokio::sync::Mutex<HashMap<String, Warm>>,
    /// Host-held credentials used by the `BOX-7` egress proxy (never placed in the sandbox).
    secrets: SecretStore,
    /// Named model-API upstreams proxied on the host (`BOX-7`).
    egress: Option<egress::EgressConfig>,
}

/// File metadata fingerprint `(size_bytes, mtime_secs)` for delta sync inside a stateful session.
type FileIndex = BTreeMap<String, (u64, i64)>;

/// A sandbox kept running between one session's tasks.
struct Warm {
    name: String,
    /// Image, resources and policy digest it was created with; a task that differs gets a new sandbox.
    shape: String,
    /// When `Some(commit)`, `/sandbox` retains state from `commit` across calls (no reset/re-upload).
    session_base: Option<String>,
    /// Last synced file index inside `/sandbox` for [`SyncMode::Delta`].
    file_index: FileIndex,
    idle_since: std::time::Instant,
}

/// A warm sandbox idle this long is deleted, and at most this many are kept (the oldest goes first).
const WARM_IDLE: Duration = Duration::from_secs(600);
const WARM_MAX: usize = 8;
const WARM_PREFIX: &str = "pbxw-";

impl OpenShellRunner {
    #[must_use]
    pub fn new(policy: SandboxPolicy) -> Self {
        Self {
            policy,
            version: std::sync::OnceLock::new(),
            warm: tokio::sync::Mutex::new(HashMap::new()),
            secrets: SecretStore::from_env(),
            egress: None,
        }
    }

    #[must_use]
    pub fn with_secrets(mut self, store: SecretStore) -> Self {
        self.secrets = store;
        self
    }

    #[must_use]
    pub fn with_egress(mut self, cfg: egress::EgressConfig) -> Self {
        self.egress = Some(cfg);
        self
    }

    /// Tighten or update a warm sandbox's live `OpenShell` network policy without restarting it.
    /// Returns `Ok(true)` if a warm sandbox existed for `session` and was updated, `Ok(false)` if none existed.
    ///
    /// # Errors
    ///
    /// Fails if `hosts` contains an invalid host entry or `openshell policy set` fails.
    pub async fn update_policy(&self, session: &str, hosts: &[String]) -> Result<bool> {
        let policy_text = policy_yaml(hosts)?;
        let target = self.warm.lock().await.get(session).map(|w| w.name.clone());
        let Some(name) = target else {
            return Ok(false);
        };
        let dir = tempdir()?;
        let policy_path = dir.join("policy.yaml");
        tokio::fs::write(&policy_path, &policy_text).await?;
        let policy_str = policy_path.to_str().context("temp path")?.to_string();
        let res = cli(
            &["policy", "set", &name, "--policy", &policy_str],
            &[],
            Duration::from_secs(30),
        )
        .await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let (out, timed_out) = res?;
        if timed_out {
            bail!("openshell policy set timed out");
        }
        ok(&out, "policy set")?;
        if let Some(w) = self.warm.lock().await.get_mut(session) {
            if let Some((prefix, _)) = w.shape.rsplit_once('|') {
                w.shape = format!("{prefix}|{}", digest(&policy_text));
            }
        }
        Ok(true)
    }
}

/// A destination parsed from `host` or `host:port` (port 443 by default). Strict, because the values
/// are written into a YAML policy: anything outside plain DNS names and IPs is refused, so a peer cannot
/// inject policy text or a wildcard.
fn parse_host(h: &str) -> Result<(String, u16)> {
    let (name, port) = match h.rsplit_once(':') {
        Some((n, p)) => (
            n,
            p.parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .with_context(|| format!("bad port in {h:?}"))?,
        ),
        None => (h, 443),
    };
    validate_hostname(name)?;
    Ok((name.to_ascii_lowercase(), port))
}

fn validate_hostname(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 253
        && name
            .split('.')
            .all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-'))
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !ok {
        bail!("invalid host {name:?}: use a plain DNS name or IPv4 address, optionally `:port`");
    }
    Ok(())
}

/// The `OpenShell` policy for one task. Pure, so it can be tested and hashed. With no hosts there are no
/// `network_policies`, so the sandbox's default-deny applies; with hosts, exactly those destinations
/// (any binary) are allowed and everything else stays denied.
///
/// `/dev/null` is permitted read-write (and `/dev/zero`, `/dev/urandom` read-only) so ordinary shell
/// redirections (`>/dev/null 2>&1`) and Python/git tooling work under Landlock hard enforcement.
///
/// # Errors
///
/// Returns an error if any host is invalid.
pub fn policy_yaml(hosts: &[String]) -> Result<String> {
    policy_yaml_with_endpoints(hosts, &[])
}

/// Like [`policy_yaml`], adding host-chosen `(host, port)` endpoints (such as the `BOX-7` egress proxy)
/// alongside any grant-allowed `hosts`. Every endpoint is strictly validated before emission.
///
/// # Errors
///
/// Returns an error if any host or port is invalid.
pub fn policy_yaml_with_endpoints(hosts: &[String], extra: &[(String, u16)]) -> Result<String> {
    let mut y = String::from(
        "version: 1\n\
         filesystem_policy:\n  include_workdir: false\n  read_only: [/usr, /lib, /bin, /sbin, /etc, /dev/zero, /dev/urandom]\n  read_write: [/sandbox, /tmp, /dev/null]\n\
         landlock:\n  compatibility: hard_requirement\n\
         process:\n  run_as_user: sandbox\n  run_as_group: sandbox\n",
    );
    let mut parsed = hosts
        .iter()
        .map(|h| parse_host(h))
        .collect::<Result<Vec<_>>>()?;
    for (h, p) in extra {
        validate_hostname(h)?;
        if *p == 0 {
            bail!("bad port 0 for host {h:?}");
        }
        parsed.push((h.to_ascii_lowercase(), *p));
    }
    if !parsed.is_empty() {
        parsed.sort();
        parsed.dedup();
        y.push_str(
            "network_policies:\n  playbox_hosts:\n    name: playbox-hosts\n    endpoints:\n",
        );
        for (h, p) in parsed {
            y.push_str(&format!("      - host: \"{h}\"\n        port: {p}\n"));
        }
        y.push_str("    binaries:\n      - path: \"/**\"\n");
    }
    Ok(y)
}

fn digest(policy: &str) -> String {
    blake3::hash(policy.as_bytes()).to_hex().to_string()
}

/// `OpenShell` takes Kubernetes-style quantities.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is bounded, or truncated on purpose; the value is clamped to be non-negative first"
)]
fn cpu_arg(cpus: f32) -> String {
    format!("{}m", ((cpus * 1000.0).round() as u32).max(1))
}

fn mem_arg(mem_mib: u32) -> String {
    format!("{mem_mib}Mi")
}

fn random_hex(n: usize) -> Result<String> {
    let mut b = vec![0u8; n];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn random_name() -> Result<String> {
    // OpenShell names are at most 19 characters
    Ok(format!("playbox-{}", random_hex(5)?))
}

fn warm_name() -> Result<String> {
    Ok(format!("{WARM_PREFIX}{}", random_hex(5)?))
}

async fn delete_sandbox(name: &str) {
    let _ = cli(&["sandbox", "delete", name], &[], Duration::from_secs(60)).await;
}

async fn cli(
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> Result<(TaskOutput, bool)> {
    let mut c = Command::new(BIN);
    c.args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    run_child(c, timeout).await
}

fn ok(out: &TaskOutput, what: &str) -> Result<()> {
    if out.exit_code != Some(0) {
        bail!("openshell {what} failed: {}", out.stderr.trim());
    }
    Ok(())
}

/// Validate a relative workspace path coming from an untrusted sandbox archive or file listing.
/// Rejects absolute paths, `..`, NUL/control bytes, backslashes, and any `.git` component.
fn validate_rel_path(raw: &str) -> Result<String> {
    if raw.contains(['\0', '\n', '\r', '\\']) {
        bail!("forbidden character in sandbox path {raw:?}");
    }
    let mut s = raw;
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    let s = s.trim_end_matches('/');
    if s.is_empty() || s == "." {
        bail!("empty sandbox path {raw:?}");
    }
    if s.starts_with('/') {
        bail!("absolute sandbox path {raw:?} refused");
    }
    let mut parts = Vec::new();
    for comp in Path::new(s).components() {
        match comp {
            Component::Normal(c) => {
                let cs = c
                    .to_str()
                    .ok_or_else(|| anyhow!("non-utf8 sandbox path {raw:?}"))?;
                if cs.eq_ignore_ascii_case(".git") {
                    bail!("sandbox path {raw:?} touches reserved .git component");
                }
                parts.push(cs);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("unsafe sandbox path traversal in {raw:?}");
            }
        }
    }
    if parts.is_empty() {
        bail!("empty normalized sandbox path {raw:?}");
    }
    Ok(parts.join("/"))
}

/// Verify that a symlink at `entry_rel` pointing to `target` stays strictly inside the workspace root
/// and does not reference `.git`.
fn validate_symlink_target(entry_rel: &str, target: &str) -> Result<()> {
    if target.is_empty() || target.starts_with('/') || target.contains(['\0', '\n', '\r', '\\']) {
        bail!("unsafe symlink target {target:?} for {entry_rel:?}");
    }
    let mut depth: Vec<&str> = entry_rel
        .rsplit_once('/')
        .map(|(p, _)| p.split('/').collect())
        .unwrap_or_default();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if depth.pop().is_none() {
                    bail!("symlink {entry_rel:?} -> {target:?} escapes workspace root");
                }
            }
            c if c.eq_ignore_ascii_case(".git") => {
                bail!("symlink {entry_rel:?} -> {target:?} references .git");
            }
            c => depth.push(c),
        }
    }
    Ok(())
}

fn cstr_field(bytes: &[u8]) -> Result<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end])
        .map(str::to_string)
        .context("non-utf8 tar header field")
}

fn parse_tar_size(field: &[u8]) -> Result<u64> {
    if field.first().is_some_and(|&b| b & 0x80 != 0) {
        // GNU base-256 extension
        let mut n = u64::from(field[0] & 0x7f);
        for &b in &field[1..] {
            n = n
                .checked_shl(8)
                .ok_or_else(|| anyhow!("tar size overflow"))?
                | u64::from(b);
        }
        return Ok(n);
    }
    let s = cstr_field(field)?;
    let t = s.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\0');
    if t.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(t, 8).with_context(|| format!("invalid tar octal size {t:?}"))
}

fn parse_pax_records(data: &[u8], path: &mut Option<String>, linkpath: &mut Option<String>) {
    let mut i = 0;
    while i < data.len() {
        let Some(sp) = data[i..].iter().position(|&b| b == b' ') else {
            break;
        };
        let Ok(len_str) = std::str::from_utf8(&data[i..i + sp]) else {
            break;
        };
        let Ok(rec_len) = len_str.parse::<usize>() else {
            break;
        };
        if rec_len == 0 || i.saturating_add(rec_len) > data.len() {
            break;
        }
        let body = &data[i + sp + 1..i + rec_len];
        let body = body.strip_suffix(b"\n").unwrap_or(body);
        if let Ok(kv) = std::str::from_utf8(body) {
            if let Some(v) = kv.strip_prefix("path=") {
                *path = Some(v.to_string());
            } else if let Some(v) = kv.strip_prefix("linkpath=") {
                *linkpath = Some(v.to_string());
            }
        }
        i += rec_len;
    }
}

/// Validate an untrusted POSIX/ustar/PAX/GNU tar stream returned from a sandbox before unpacking it
/// on the host. Refuses absolute paths, `..`, `.git`, hardlinks, device nodes, FIFOs, and symlinks
/// escaping the workspace root. Returns normalized relative file/dir entries.
fn validate_tar_bytes(bytes: &[u8]) -> Result<Vec<String>> {
    let mut pos = 0usize;
    let mut entries = Vec::new();
    let mut pax_path: Option<String> = None;
    let mut pax_link: Option<String> = None;
    let mut gnu_name: Option<String> = None;
    let mut gnu_link: Option<String> = None;

    while pos + 512 <= bytes.len() {
        let hdr = &bytes[pos..pos + 512];
        pos += 512;
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        let size = parse_tar_size(&hdr[124..136])?;
        let data_len = usize::try_from(size).context("tar entry size exceeds address space")?;
        let padded = data_len
            .checked_add(511)
            .ok_or_else(|| anyhow!("tar entry size overflow"))?
            & !511;
        if pos.checked_add(padded).is_none_or(|end| end > bytes.len()) {
            bail!("truncated tar archive");
        }
        let payload = &bytes[pos..pos + data_len];
        pos += padded;

        let kind = hdr[156];
        match kind {
            b'x' => {
                parse_pax_records(payload, &mut pax_path, &mut pax_link);
                continue;
            }
            b'g' => continue,
            b'L' => {
                gnu_name = Some(cstr_field(payload)?);
                continue;
            }
            b'K' => {
                gnu_link = Some(cstr_field(payload)?);
                continue;
            }
            _ => {}
        }

        let name = if let Some(p) = pax_path.take().or_else(|| gnu_name.take()) {
            p
        } else {
            let base = cstr_field(&hdr[0..100])?;
            let prefix = cstr_field(&hdr[345..500])?;
            if prefix.is_empty() {
                base
            } else {
                format!("{prefix}/{base}")
            }
        };
        let link = pax_link
            .take()
            .or_else(|| gnu_link.take())
            .unwrap_or_else(|| cstr_field(&hdr[157..257]).unwrap_or_default());

        match kind {
            0 | b'0' | b'5' => {
                let trimmed = name.trim_end_matches('/');
                if trimmed == "." || trimmed.is_empty() {
                    if kind == b'5' {
                        continue;
                    }
                    bail!("regular tar entry has empty/dot name");
                }
                entries.push(validate_rel_path(&name)?);
            }
            b'2' => {
                let norm = validate_rel_path(&name)?;
                validate_symlink_target(&norm, &link)?;
                entries.push(norm);
            }
            b'1' => bail!("hardlink tar entry {name:?} refused"),
            b'3' | b'4' | b'6' => bail!("special device/fifo tar entry {name:?} refused"),
            other => bail!(
                "unsupported tar entry type {:?} for {name:?}",
                other as char
            ),
        }
    }
    Ok(entries)
}

/// Parse guest `stat -c '%s %Y %n'` lines into a validated relative-path index.
fn parse_file_index(stdout: &str) -> Result<FileIndex> {
    let mut out = FileIndex::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ' ');
        let size: u64 = parts
            .next()
            .context("missing size")?
            .parse()
            .context("bad file size")?;
        let mtime: i64 = parts
            .next()
            .context("missing mtime")?
            .parse()
            .context("bad file mtime")?;
        let raw_path = parts.next().context("missing path")?;
        let rel = validate_rel_path(raw_path)?;
        out.insert(rel, (size, mtime));
    }
    Ok(out)
}

/// Compute `(added_or_modified, deleted)` relative paths between two snapshots.
fn diff_file_index(prev: &FileIndex, curr: &FileIndex) -> (Vec<String>, Vec<String>) {
    let mut changed = Vec::new();
    for (p, sig) in curr {
        if prev.get(p) != Some(sig) {
            changed.push(p.clone());
        }
    }
    let mut deleted = Vec::new();
    for p in prev.keys() {
        if !curr.contains_key(p) {
            deleted.push(p.clone());
        }
    }
    (changed, deleted)
}

/// Safely remove a relative path inside `root`, verifying no intermediate component is a symlink.
fn safe_remove_rel(root: &Path, rel: &str) -> Result<()> {
    let norm = validate_rel_path(rel)?;
    let mut cur = root.to_path_buf();
    let comps: Vec<&str> = norm.split('/').collect();
    for &c in &comps[..comps.len().saturating_sub(1)] {
        cur.push(c);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("refusing to traverse symlink parent {}", cur.display());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
    cur.push(comps[comps.len() - 1]);
    match std::fs::symlink_metadata(&cur) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
            std::fs::remove_dir_all(&cur)?;
        }
        Ok(_) => {
            std::fs::remove_file(&cur)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Shell single-quote a string safely for POSIX `sh`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Read the host-side bind address and guest-visible hostname for the `BOX-7` egress proxy.
fn egress_endpoints() -> Result<(Ipv4Addr, String)> {
    let host = std::env::var("OPENSHELL_EGRESS_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "host.docker.internal".to_string());
    validate_hostname(&host)?;
    let bind_ip = match std::env::var("OPENSHELL_EGRESS_BIND")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        Some(raw) => raw
            .parse::<Ipv4Addr>()
            .with_context(|| format!("invalid OPENSHELL_EGRESS_BIND IPv4 address {raw:?}"))?,
        None => Ipv4Addr::LOCALHOST,
    };
    Ok((bind_ip, host.to_ascii_lowercase()))
}

#[async_trait]
impl Runner for OpenShellRunner {
    fn name(&self) -> &'static str {
        "openshell"
    }

    fn isolation(&self) -> Isolation {
        Isolation::Container
    }

    fn enforces_host_list(&self) -> bool {
        true
    }

    /// The `OpenShell` version and the digest of the no-network base policy, so the host's signed
    /// statement changes when either changes.
    fn attestation(&self) -> String {
        let base = policy_yaml(&[]).unwrap_or_else(|e| format!("invalid:{e}"));
        format!(
            "{};base-policy={};{}",
            self.version.get().map_or("unknown", String::as_str),
            digest(&base),
            self.policy.describe()
        )
    }

    async fn preflight(&self) -> Result<()> {
        let (out, _) = cli(&["--version"], &[], Duration::from_secs(10))
            .await
            .context("openshell is not installed (https://github.com/NVIDIA/OpenShell)")?;
        ok(&out, "--version")?;
        let _ = self.version.set(out.stdout.trim().to_string());
        let (out, _) = cli(&["sandbox", "list"], &[], Duration::from_secs(20)).await?;
        if out.exit_code != Some(0) {
            bail!("openshell gateway is not reachable: {}", out.stderr.trim());
        }
        // Warm sandboxes outlive a host that stopped without cleaning up; nothing holds them now.
        for name in out
            .stdout
            .split_whitespace()
            .filter(|w| w.starts_with(WARM_PREFIX))
        {
            delete_sandbox(name).await;
        }
        if !crate::env_opt_in("PLAYBOX_ALLOW_MISSING_LIMITS") {
            bail!(
                "OpenShell policies have no CPU, memory or timeout limits and its VM driver ignores --cpu/--memory, \
                 so a task could exceed what its grant allows. Use a gateway driver that enforces them (e.g. Kubernetes) \
                 and set PLAYBOX_ALLOW_MISSING_LIMITS=1 to confirm, or use the docker or libkrun runner."
            );
        }
        Ok(())
    }

    async fn image_digest(&self, image: &str) -> Result<[u8; 32]> {
        if image.starts_with('-') || image.contains(char::is_whitespace) {
            bail!("invalid image name");
        }
        let out = Command::new("docker")
            .args(["image", "inspect", "--format", "{{.Id}}", image])
            .stdin(Stdio::null())
            .output()
            .await
            .context("docker image inspect (for openshell image digest)")?;
        if !out.status.success() {
            bail!(
                "cannot inspect openshell image {image:?}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        parse_image_id(String::from_utf8_lossy(&out.stdout).trim())
    }

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        self.execute(spec, workspace, None, None, SyncMode::Full)
            .await
    }

    async fn run_warm(
        &self,
        spec: &TaskSpec,
        workspace: &Path,
        session: &str,
    ) -> Result<TaskOutput> {
        self.execute(spec, workspace, Some(session), None, SyncMode::Full)
            .await
    }

    async fn run_session(
        &self,
        spec: &TaskSpec,
        workspace: &Path,
        session: &str,
        base_commit: &str,
        sync: SyncMode,
    ) -> Result<TaskOutput> {
        self.execute(spec, workspace, Some(session), Some(base_commit), sync)
            .await
    }

    async fn end_warm(&self, session: &str) {
        let w = self.warm.lock().await.remove(session);
        if let Some(w) = w {
            delete_sandbox(&w.name).await;
        }
    }
}

impl OpenShellRunner {
    /// Delete warm sandboxes idle too long, and the oldest ones beyond the cap (`room` makes space for one more).
    async fn reap(&self, room: bool) {
        let mut gone = Vec::new();
        {
            let mut m = self.warm.lock().await;
            let now = std::time::Instant::now();
            let stale: Vec<String> = m
                .iter()
                .filter(|(_, w)| now.duration_since(w.idle_since) > WARM_IDLE)
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale {
                gone.extend(m.remove(&k));
            }
            while m.len() >= WARM_MAX && room {
                let Some(oldest) = m
                    .iter()
                    .min_by_key(|(_, w)| w.idle_since)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                gone.extend(m.remove(&oldest));
            }
        }
        for w in gone {
            delete_sandbox(&w.name).await;
        }
    }

    async fn execute(
        &self,
        spec: &TaskSpec,
        workspace: &Path,
        session: Option<&str>,
        session_base: Option<&str>,
        sync: SyncMode,
    ) -> Result<TaskOutput> {
        // Validate spec: OpenShell honours allowed_hosts, and BOX-7 egress when configured.
        let mut plain = spec.clone();
        plain.allowed_hosts.clear();
        if self.egress.is_some() {
            plain.egress.clear();
        }
        check_spec(&plain)?;
        self.policy.check(spec)?;
        if spec.network && spec.allowed_hosts.is_empty() && spec.egress.is_empty() {
            bail!("the openshell runner only allows network to named hosts (allowed_hosts) or host-proxied egress upstreams; unrestricted network is refused");
        }

        let mut extra_endpoints = Vec::new();
        let egress_proxy = if spec.egress.is_empty() {
            None
        } else {
            let cfg = self
                .egress
                .as_ref()
                .ok_or_else(|| anyhow!("this openshell host has no egress upstreams configured"))?;
            let (bind_ip, guest_host) = egress_endpoints()?;
            let proxy = egress::Proxy::start_with_host(
                cfg,
                &self.secrets,
                &spec.egress,
                bind_ip,
                &guest_host,
            )
            .await?;
            extra_endpoints.push((guest_host, proxy.port()));
            Some(proxy)
        };

        let policy_text = policy_yaml_with_endpoints(&spec.allowed_hosts, &extra_endpoints)?;
        let ws = workspace.to_str().context("workspace path is not utf-8")?;
        let shape = format!(
            "{}|{}|{}|{}",
            spec.image,
            cpu_arg(spec.cpus),
            mem_arg(spec.mem_mib),
            digest(&policy_text)
        );

        let dir = tempdir()?;
        let policy_path = dir.join("policy.yaml");
        tokio::fs::write(&policy_path, &policy_text).await?;
        let policy_path = policy_path.to_str().context("temp path")?.to_string();

        // A warm sandbox is taken out of the map while it runs, so one session never runs two tasks in it at once.
        let mut reused: Option<(String, bool, FileIndex)> = None;
        if let Some(sess) = session {
            self.reap(false).await;
            let held = self.warm.lock().await.remove(sess);
            if let Some(w) = held {
                let same_shape = w.shape == shape;
                let stateful_hit = same_shape
                    && session_base.is_some()
                    && w.session_base.as_deref() == session_base;
                if stateful_hit {
                    // Stateful persistent session: keep /sandbox intact and skip upload!
                    reused = Some((w.name, false, w.file_index));
                } else if same_shape && self.reset(&w.name).await.is_ok() {
                    reused = Some((w.name, true, FileIndex::new()));
                } else {
                    delete_sandbox(&w.name).await;
                }
            }
        }
        let (name, needs_upload, mut file_index) = match reused {
            Some(r) => r,
            None => {
                let n = if session.is_some() {
                    warm_name()?
                } else {
                    random_name()?
                };
                if let Err(e) = self.create(spec, &n, &policy_path).await {
                    delete_sandbox(&n).await;
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    return Err(e);
                }
                (n, true, FileIndex::new())
            }
        };

        let egress_envs = egress_proxy
            .as_ref()
            .map(egress::Proxy::env_vars)
            .unwrap_or(&[]);
        let result = self
            .exec_in(
                spec,
                ws,
                &name,
                &dir,
                needs_upload,
                sync,
                &mut file_index,
                egress_envs,
            )
            .await
            .map(|(mut o, timed_out)| {
                o.policy_digest = Some(digest(&policy_text));
                if let Some(proxy) = &egress_proxy {
                    o.secrets_used.extend_from_slice(proxy.secrets_used());
                }
                (o, timed_out)
            });

        // Keep the sandbox only after a clean run in a session with no ephemeral per-task egress port.
        let keep = session.is_some() && egress_proxy.is_none() && matches!(result, Ok((_, false)));
        if keep {
            self.reap(true).await;
            if let Some(sess) = session {
                self.warm.lock().await.insert(
                    sess.to_string(),
                    Warm {
                        name: name.clone(),
                        shape,
                        session_base: session_base.map(str::to_string),
                        file_index,
                        idle_since: std::time::Instant::now(),
                    },
                );
            }
        } else {
            delete_sandbox(&name).await;
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
        result.map(|(o, _)| o)
    }

    /// Empty what a previous task left in the places the task user can write, so the next one starts from the
    /// workspace it is given and nothing else. Processes the previous task left running are not touched.
    async fn reset(&self, name: &str) -> Result<()> {
        let (out, timed_out) = cli(
            &[
                "sandbox",
                "exec",
                "-n",
                name,
                "--no-tty",
                "--no-login-shell",
                "--",
                "sh",
                "-c",
                "find /sandbox /tmp -mindepth 1 -delete; true",
            ],
            &[],
            Duration::from_secs(60),
        )
        .await?;
        if timed_out {
            bail!("resetting the warm sandbox timed out");
        }
        ok(&out, "reset")
    }
}

/// Where the workspace lives inside the sandbox: its default working directory, which the `sandbox`
/// user can write (`/workspace` cannot be created by a non-root user).
const GUEST_DIR: &str = "/sandbox";

const MAX_STREAM_BYTES: u64 = 256 * 1024 * 1024;

/// Run a command with `input` on stdin and return its raw stdout (a tar stream, so not lossy-decoded).
async fn piped(
    mut cmd: Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<(Option<i32>, Vec<u8>, String)> {
    use tokio::io::AsyncWriteExt;
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn")?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(&data).await;
        });
    }
    let mut stdout = child.stdout.take().context("child stdout was not piped")?;
    let mut stderr = child.stderr.take().context("child stderr was not piped")?;
    let run = async {
        let out_fut = async {
            let mut o = Vec::new();
            (&mut stdout)
                .take(MAX_STREAM_BYTES + 1)
                .read_to_end(&mut o)
                .await?;
            if o.len() as u64 > MAX_STREAM_BYTES {
                let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
                bail!(
                    "sandbox stream exceeds {} MiB limit",
                    MAX_STREAM_BYTES >> 20
                );
            }
            Ok::<_, anyhow::Error>(o)
        };
        let err_fut = async {
            let mut e = Vec::new();
            (&mut stderr)
                .take(super::MAX_OUTPUT as u64)
                .read_to_end(&mut e)
                .await?;
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            Ok::<_, anyhow::Error>(e)
        };
        let (a, b) = tokio::join!(out_fut, err_fut);
        let o = a?;
        let e = b?;
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((status.code(), o, String::from_utf8_lossy(&e).into_owned()))
    };
    tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| anyhow::anyhow!("timed out"))?
}

/// `openshell sandbox exec` refuses more than 4 MiB on stdin ("use `sandbox upload` for larger input"), so a bigger
/// workspace goes in as several stdin chunks appended to a tar file in the sandbox's `/tmp`, which is unpacked and
/// removed before the task's command starts.
const STDIN_CHUNK: usize = 3 * 1024 * 1024;
/// Longest single argument sent on an `exec` command line (OpenShell rejects more than 32 KiB).
const MAX_ARG: usize = 24 * 1024;
const GUEST_SCRIPT: &str = "/tmp/.pbx-task.sh";
const GUEST_TAR: &str = "/tmp/.pbx-workspace.tar";

/// Pack a host workspace cleanly without macOS extended attributes or `.git` worktree pointer leaks.
async fn pack_workspace(ws: &str) -> Result<Vec<u8>> {
    let mut cmd = Command::new("tar");
    cmd.env("COPYFILE_DISABLE", "1");
    #[cfg(target_os = "macos")]
    cmd.args(["--no-xattrs", "--no-mac-metadata"]);
    cmd.args([
        "--exclude=./.git",
        "--exclude=.git",
        "-cf",
        "-",
        "-C",
        ws,
        ".",
    ])
    .stdin(Stdio::null());
    let packed = cmd.output().await?;
    if !packed.status.success() {
        bail!(
            "packing the workspace failed: {}",
            String::from_utf8_lossy(&packed.stderr).trim()
        );
    }
    Ok(packed.stdout)
}

/// Validate and unpack an untrusted tar archive from the sandbox into a fresh empty directory.
async fn unpack_validated_tar(tar: &[u8], out_dir: &Path) -> Result<Vec<String>> {
    let entries = validate_tar_bytes(tar)?;
    tokio::fs::create_dir_all(out_dir).await?;
    if entries.is_empty() {
        return Ok(entries);
    }
    let mut un = Command::new("tar")
        .args(["-xf", "-", "--no-same-owner", "--no-same-permissions", "-C"])
        .arg(out_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = un.stdin.take().context("child stdin was not piped")?;
        stdin.write_all(tar).await?;
    }
    let un = un.wait_with_output().await?;
    if !un.status.success() {
        bail!(
            "unpacking the workspace failed: {}",
            String::from_utf8_lossy(&un.stderr).trim()
        );
    }
    Ok(entries)
}

impl OpenShellRunner {
    async fn exec_stdin(&self, name: &str, script: &[&str], data: Vec<u8>) -> Result<()> {
        let mut args = vec![
            "sandbox",
            "exec",
            "-n",
            name,
            "--no-tty",
            "--no-login-shell",
            "--",
        ];
        args.extend_from_slice(script);
        let (code, _, err) = piped(osh(&args), Some(data), SETUP_TIMEOUT).await?;
        if code != Some(0) {
            bail!("uploading to sandbox failed: {}", err.trim());
        }
        Ok(())
    }

    async fn upload(&self, name: &str, tar: Vec<u8>) -> Result<()> {
        if tar.len() <= STDIN_CHUNK {
            return self
                .exec_stdin(name, &["tar", "-xf", "-", "-C", GUEST_DIR], tar)
                .await;
        }
        for (i, chunk) in tar.chunks(STDIN_CHUNK).enumerate() {
            let redirect = if i == 0 { ">" } else { ">>" };
            let script = format!("cat {redirect} {GUEST_TAR}");
            self.exec_stdin(name, &["sh", "-c", &script], chunk.to_vec())
                .await?;
        }
        let unpack =
            format!("tar -xf {GUEST_TAR} -C {GUEST_DIR}; rc=$?; rm -f {GUEST_TAR}; exit $rc");
        self.exec_stdin(name, &["sh", "-c", &unpack], Vec::new())
            .await
    }

    /// Scan `/sandbox` inside the guest into a `(rel_path -> (size, mtime))` index for delta sync.
    async fn scan_guest_files(&self, name: &str) -> Result<FileIndex> {
        let script = "cd /sandbox && find . -path './.git' -prune -o \\( -type f -o -type l \\) -exec stat -c '%s %Y %n' {} +";
        let (code, raw, err) = piped(
            osh(&[
                "sandbox",
                "exec",
                "-n",
                name,
                "--no-tty",
                "--no-login-shell",
                "--",
                "sh",
                "-c",
                script,
            ]),
            None,
            Duration::from_secs(30),
        )
        .await?;
        if code != Some(0) {
            bail!("scanning sandbox files failed: {}", err.trim());
        }
        let text = std::str::from_utf8(&raw).context("non-utf8 sandbox file listing")?;
        parse_file_index(text)
    }

    /// Mirror the full sandbox tree safely back into `ws` (never touching host `.git`).
    async fn sync_full(&self, name: &str, ws: &str, tmp: &Path) -> Result<()> {
        let (code, tar, err) = piped(
            osh(&[
                "sandbox",
                "exec",
                "-n",
                name,
                "--no-tty",
                "--no-login-shell",
                "--",
                "tar",
                "--exclude=./.git",
                "--exclude=.git",
                "-cf",
                "-",
                "-C",
                GUEST_DIR,
                ".",
            ]),
            None,
            SETUP_TIMEOUT,
        )
        .await?;
        if code != Some(0) {
            bail!("downloading the workspace failed: {}", err.trim());
        }
        let out_dir = tmp.join("out");
        let _ = unpack_validated_tar(&tar, &out_dir).await?;
        let sync = Command::new("rsync")
            .args(["-a", "--delete", "--exclude=/.git"])
            .arg(format!("{}/", out_dir.display()))
            .arg(format!("{ws}/"))
            .stdin(Stdio::null())
            .output()
            .await?;
        if !sync.status.success() {
            bail!(
                "copying the workspace back failed: {}",
                String::from_utf8_lossy(&sync.stderr).trim()
            );
        }
        Ok(())
    }

    /// Mirror only files changed or deleted since `file_index` back into `ws`. Falls back to
    /// [`Self::sync_full`] if scanning or targeted tar fails.
    async fn sync_delta(
        &self,
        name: &str,
        ws: &str,
        tmp: &Path,
        file_index: &mut FileIndex,
    ) -> Result<()> {
        let Ok(curr) = self.scan_guest_files(name).await else {
            self.sync_full(name, ws, tmp).await?;
            if let Ok(fresh) = self.scan_guest_files(name).await {
                *file_index = fresh;
            }
            return Ok(());
        };
        let (changed, deleted) = diff_file_index(file_index, &curr);
        if changed.is_empty() && deleted.is_empty() {
            return Ok(());
        }
        if !changed.is_empty() {
            let list_bytes = (changed.join("\n") + "\n").into_bytes();
            let tar_res = if list_bytes.len() <= STDIN_CHUNK {
                piped(
                    osh(&[
                        "sandbox",
                        "exec",
                        "-n",
                        name,
                        "--no-tty",
                        "--no-login-shell",
                        "--",
                        "tar",
                        "-cf",
                        "-",
                        "-C",
                        GUEST_DIR,
                        "-T",
                        "-",
                    ]),
                    Some(list_bytes),
                    SETUP_TIMEOUT,
                )
                .await
            } else {
                Err(anyhow!("delta path list exceeds stdin chunk"))
            };
            match tar_res {
                Ok((Some(0), tar, _)) => {
                    let out_dir = tmp.join("delta");
                    let _ = unpack_validated_tar(&tar, &out_dir).await?;
                    let sync = Command::new("rsync")
                        .args(["-a", "--exclude=/.git"])
                        .arg(format!("{}/", out_dir.display()))
                        .arg(format!("{ws}/"))
                        .stdin(Stdio::null())
                        .output()
                        .await?;
                    if !sync.status.success() {
                        bail!(
                            "copying workspace delta back failed: {}",
                            String::from_utf8_lossy(&sync.stderr).trim()
                        );
                    }
                }
                _ => {
                    self.sync_full(name, ws, tmp).await?;
                    *file_index = curr;
                    return Ok(());
                }
            }
        }
        let ws_path = Path::new(ws);
        for rel in &deleted {
            safe_remove_rel(ws_path, rel)?;
        }
        *file_index = curr;
        Ok(())
    }
}

fn osh(args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args);
    c
}

impl OpenShellRunner {
    async fn create(&self, spec: &TaskSpec, name: &str, policy: &str) -> Result<()> {
        let (cpu, mem) = (cpu_arg(spec.cpus), mem_arg(spec.mem_mib));
        let (out, timed_out) = cli(
            &[
                "sandbox",
                "create",
                "--name",
                name,
                "--from",
                &spec.image,
                "--cpu",
                &cpu,
                "--memory",
                &mem,
                "--policy",
                policy,
                "--detach",
            ],
            &[],
            SETUP_TIMEOUT,
        )
        .await?;
        if timed_out {
            bail!("openshell sandbox create timed out");
        }
        ok(&out, "sandbox create")
    }

    /// Upload the workspace (when `needs_upload`), run the command, and mirror workspace changes according to `sync`.
    #[allow(clippy::too_many_arguments)]
    async fn exec_in(
        &self,
        spec: &TaskSpec,
        ws: &str,
        name: &str,
        tmp: &Path,
        needs_upload: bool,
        sync: SyncMode,
        file_index: &mut FileIndex,
        egress_envs: &[(String, String)],
    ) -> Result<(TaskOutput, bool)> {
        if needs_upload {
            let packed = pack_workspace(ws).await?;
            self.upload(name, packed).await?;
            if sync == SyncMode::Delta {
                *file_index = self.scan_guest_files(name).await.unwrap_or_default();
            }
        }

        // Stage BOX-7 egress capability URLs into a 0600 file in /tmp via stdin (never in host/guest argv),
        // then source and unlink it immediately before exec'ing the task command.
        let env_file = if egress_envs.is_empty() {
            None
        } else {
            let path = format!("/tmp/.pbx-env-{}", random_hex(8)?);
            let mut body = String::new();
            for (k, v) in egress_envs {
                body.push_str(&format!("export {k}={}\n", sh_quote(v)));
            }
            let setup = format!("umask 077 && cat > {path}");
            self.exec_stdin(name, &["sh", "-c", &setup], body.into_bytes())
                .await?;
            Some(path)
        };

        let secs = spec.timeout_secs.to_string();
        let mut args = vec![
            "sandbox",
            "exec",
            "-n",
            name,
            "--no-tty",
            "--no-login-shell",
            "--workdir",
            GUEST_DIR,
            "--timeout",
            &secs,
            "--",
        ];
        // `exec` also refuses a single argument over 32 KiB. A long `sh -c` script (an edit that carries a whole file, say)
        // is staged as a file in the sandbox's /tmp instead and run from there; same shell, same user, same policy.
        let staged_vec: Vec<String>;
        let cmd_refs: Vec<&str> = if let Some(ef) = &env_file {
            let inner_cmd = match spec.cmd.as_slice() {
                [sh, flag, script] if sh == "sh" && flag == "-c" && script.len() > MAX_ARG => {
                    self.exec_stdin(
                        name,
                        &["sh", "-c", &format!("cat > {GUEST_SCRIPT}")],
                        script.clone().into_bytes(),
                    )
                    .await?;
                    format!("exec {} {}", sh_quote(sh), sh_quote(GUEST_SCRIPT))
                }
                _ => {
                    let quoted: Vec<String> = spec.cmd.iter().map(|s| sh_quote(s)).collect();
                    format!("exec {}", quoted.join(" "))
                }
            };
            let wrapper = format!(". {ef}; rm -f {ef}; {inner_cmd}");
            if wrapper.len() > MAX_ARG {
                self.exec_stdin(
                    name,
                    &["sh", "-c", &format!("cat > {GUEST_SCRIPT}")],
                    wrapper.into_bytes(),
                )
                .await?;
                staged_vec = vec!["sh".into(), GUEST_SCRIPT.into()];
            } else {
                staged_vec = vec!["sh".into(), "-c".into(), wrapper];
            }
            staged_vec.iter().map(String::as_str).collect()
        } else {
            match spec.cmd.as_slice() {
                [sh, flag, script] if sh == "sh" && flag == "-c" && script.len() > MAX_ARG => {
                    self.exec_stdin(
                        name,
                        &["sh", "-c", &format!("cat > {GUEST_SCRIPT}")],
                        script.clone().into_bytes(),
                    )
                    .await?;
                    staged_vec = vec![sh.clone(), GUEST_SCRIPT.into()];
                    staged_vec.iter().map(String::as_str).collect()
                }
                _ => spec.cmd.iter().map(String::as_str).collect(),
            }
        };
        args.extend(cmd_refs);
        // A little slack so OpenShell's own timeout reports first; ours is the backstop.
        let (task, timed_out) =
            cli(&args, &[], Duration::from_secs(spec.timeout_secs + 10)).await?;

        // Workspace out, so the host can diff it. Skipped on timeout (state is partial) or SyncMode::None.
        if !timed_out {
            match sync {
                SyncMode::None => {}
                SyncMode::Delta => {
                    self.sync_delta(name, ws, tmp, file_index).await?;
                }
                SyncMode::Full => {
                    self.sync_full(name, ws, tmp).await?;
                    if needs_upload {
                        *file_index = self.scan_guest_files(name).await.unwrap_or_default();
                    }
                }
            }
        }
        Ok((task, timed_out))
    }
}

fn tempdir() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    for _ in 0..32 {
        let hex = random_hex(16)?;
        let p = base.join(format!("playbox-osh-{hex}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    bail!("could not create an atomic scratch directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tar_entry(name: &str, kind: u8, linkname: &str, payload: &[u8]) -> Vec<u8> {
        let mut hdr = [0u8; 512];
        let nb = name.as_bytes();
        hdr[..nb.len()].copy_from_slice(nb);
        let size_oct = format!("{:011o}\0", payload.len());
        hdr[124..136].copy_from_slice(size_oct.as_bytes());
        hdr[156] = kind;
        let lb = linkname.as_bytes();
        hdr[157..157 + lb.len()].copy_from_slice(lb);
        hdr[257..263].copy_from_slice(b"ustar\0");
        hdr[263..265].copy_from_slice(b"00");
        let mut out = hdr.to_vec();
        out.extend_from_slice(payload);
        let pad = (512 - (payload.len() % 512)) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
        out
    }

    fn finish_tar(mut data: Vec<u8>) -> Vec<u8> {
        data.extend(std::iter::repeat_n(0u8, 1024));
        data
    }

    #[test]
    fn policy_has_no_network_by_default_and_requires_landlock_with_dev_null() {
        let p = policy_yaml(&[]).unwrap();
        assert!(p.starts_with("version: 1"));
        assert!(!p.contains("network_policies"));
        assert!(p.contains("hard_requirement") && p.contains("run_as_user: sandbox"));
        assert!(
            p.contains("read_write: [/sandbox, /tmp, /dev/null]"),
            "Landlock policy must allow /dev/null redirections: {p}"
        );
        assert!(p.contains("/dev/urandom"));
    }

    #[test]
    fn hosts_become_exact_endpoints_and_cannot_inject_policy() {
        let p = policy_yaml(&[
            "API.github.com".into(),
            "example.com:8443".into(),
            "api.github.com".into(),
        ])
        .unwrap();
        assert!(p.contains("network_policies"));
        assert_eq!(p.matches("- host:").count(), 2, "{p}");
        assert!(p.contains("\"api.github.com\"") && p.contains("port: 8443"));
        for bad in [
            "*.example.com",
            "a.com\"\n  evil: x",
            "a b",
            "",
            "a..com",
            "-a.com",
            "a.com:0",
            "a.com:99999",
            "a.com:x",
            "a/b",
        ] {
            assert!(
                policy_yaml(&[bad.into()]).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn policy_with_extra_egress_endpoint_allows_only_proxy_port() {
        let p = policy_yaml_with_endpoints(&[], &[("host.docker.internal".into(), 45123)]).unwrap();
        assert!(p.contains("network_policies"));
        assert!(p.contains("\"host.docker.internal\""));
        assert!(p.contains("port: 45123"));
        assert_eq!(p.matches("- host:").count(), 1);
        assert!(policy_yaml_with_endpoints(&[], &[("host.docker.internal".into(), 0)]).is_err());
        assert!(policy_yaml_with_endpoints(&[], &[("bad host".into(), 443)]).is_err());
    }

    #[test]
    fn digest_changes_with_hosts() {
        assert_ne!(
            digest(&policy_yaml(&[]).unwrap()),
            digest(&policy_yaml(&["a.com".into()]).unwrap())
        );
    }

    #[test]
    fn quantities_are_kubernetes_style() {
        assert_eq!(cpu_arg(0.5), "500m");
        assert_eq!(cpu_arg(2.0), "2000m");
        assert_eq!(mem_arg(512), "512Mi");
    }

    #[test]
    fn untrusted_tar_validator_accepts_safe_files_and_rejects_escapes() {
        let mut good = Vec::new();
        good.extend(make_tar_entry("./", b'5', "", b""));
        good.extend(make_tar_entry("./src/app.py", b'0', "", b"print(1)\n"));
        good.extend(make_tar_entry("src/link.py", b'2', "app.py", b""));
        good.extend(make_tar_entry("a/b/rel.py", b'2', "../c.py", b""));
        let entries = validate_tar_bytes(&finish_tar(good)).unwrap();
        assert_eq!(entries, vec!["src/app.py", "src/link.py", "a/b/rel.py"]);

        // Every hostile archive shape must fail closed.
        let bad_cases = [
            make_tar_entry("/etc/passwd", b'0', "", b"x"),
            make_tar_entry("../escape.txt", b'0', "", b"x"),
            make_tar_entry("a/../../escape.txt", b'0', "", b"x"),
            make_tar_entry(".git/config", b'0', "", b"[core]\nfsmonitor=evil"),
            make_tar_entry("sub/.GIT/hooks/pre-commit", b'0', "", b"#!/bin/sh"),
            make_tar_entry("evil_link", b'2', "/etc/passwd", b""),
            make_tar_entry("a/evil_rel", b'2', "../../etc/passwd", b""),
            make_tar_entry("a/git_link", b'2', "../.git/config", b""),
            make_tar_entry("hard", b'1', "src/app.py", b""),
            make_tar_entry("fifo", b'6', "", b""),
            make_tar_entry("chardev", b'3', "", b""),
        ];
        for case in bad_cases {
            assert!(
                validate_tar_bytes(&finish_tar(case)).is_err(),
                "hostile tar entry must be rejected"
            );
        }
    }

    #[test]
    fn delta_index_detects_added_modified_deleted_and_rejects_git() {
        let prev = parse_file_index("10 100 ./a.py\n20 100 ./b.py\n30 100 ./gone.py\n").unwrap();
        let curr = parse_file_index("10 100 ./a.py\n25 101 ./b.py\n5 102 ./new.py\n").unwrap();
        let (changed, deleted) = diff_file_index(&prev, &curr);
        assert_eq!(changed, vec!["b.py", "new.py"]);
        assert_eq!(deleted, vec!["gone.py"]);
        assert!(parse_file_index("10 100 ./.git/config\n").is_err());
        assert!(parse_file_index("10 100 ./../escape\n").is_err());
    }

    #[tokio::test]
    async fn refuses_unrestricted_network_unlisted_images_and_unconfigured_egress() {
        let r = OpenShellRunner::new(SandboxPolicy::default());
        let mut spec = TaskSpec::new(&["true"]);
        spec.network = true;
        assert!(r.run(&spec, Path::new("/tmp")).await.is_err());
        let mut spec = TaskSpec::new(&["true"]);
        spec.image = "evil/miner".into();
        assert!(r.run(&spec, Path::new("/tmp")).await.is_err());
        let mut spec = TaskSpec::new(&["true"]);
        spec.egress = vec!["anthropic".into()];
        assert!(r.run(&spec, Path::new("/tmp")).await.is_err());
    }
}
