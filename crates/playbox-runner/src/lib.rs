//! Task runners. A runner executes one command against a workspace directory.
//!
//! - [`DockerRunner`]: container isolation (not a microVM). Usable today on macOS and Linux.
//! - [`KrunRunner`]: the libkrun microVM runner (needs the `playbox-krun-helper` binary; see `scripts/install-krun.sh`).
//! - [`ProcessRunner`]: runs on the host with NO isolation. Development only, off unless asked for.

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{os::unix::fs::MetadataExt, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

#[cfg(target_os = "linux")]
pub mod cgroup;
#[cfg(target_os = "linux")]
pub mod jail;
pub mod netpolicy;
pub mod workspace;

/// How strongly a backend separates a task from the host. Ordered: a host can demand a minimum
/// and refuse to start with anything weaker, so a missing backend never degrades silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Isolation {
    /// Runs on the host itself.
    None,
    /// Shared kernel, namespaces and cgroups (Docker, gVisor-less runc).
    Container,
    /// Own guest kernel behind a hypervisor (libkrun, Firecracker).
    MicroVm,
    /// A microVM whose memory the host cannot read (SEV-SNP, TDX).
    ConfidentialVm,
}

/// Host-side rules for what a peer may ask a sandbox to run. Peers never widen these.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Exact image references a task may use. An entry may be a tag or a `name@sha256:..` digest.
    pub allowed_images: Vec<String>,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            allowed_images: vec![DEFAULT_IMAGE.into()],
        }
    }
}

impl SandboxPolicy {
    /// Canonical text of the image allow-list, for the host's signed policy measurement: a verifier
    /// that expects one image set sees a different digest if the host allows another.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut v = self.allowed_images.clone();
        v.sort();
        format!("images={}", v.join(","))
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn check(&self, spec: &TaskSpec) -> Result<()> {
        if !self.allowed_images.iter().any(|i| i == &spec.image) {
            bail!(
                "image {:?} is not allowed on this host (allowed: {})",
                spec.image,
                self.allowed_images.join(", ")
            );
        }
        Ok(())
    }
}

mod krun;
pub use krun::{find_helper, KrunRunner};
mod openshell;
pub use openshell::{policy_yaml as openshell_policy_yaml, OpenShellRunner};

pub const DEFAULT_IMAGE: &str = "alpine:3";

const MAX_OUTPUT: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskSpec {
    pub cmd: Vec<String>,
    pub image: String,
    pub cpus: f32,
    pub mem_mib: u32,
    pub timeout_secs: u64,
    /// Unrestricted network. Ignored (and implied) when `allowed_hosts` is non-empty.
    pub network: bool,
    /// Network limited to exactly these destinations (`host` or `host:port`, port 443 by default).
    /// Only runners that can enforce it accept a non-empty list; the others refuse the task.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl TaskSpec {
    #[must_use]
    pub fn new(cmd: &[&str]) -> Self {
        Self {
            cmd: cmd.iter().map(std::string::ToString::to_string).collect(),
            image: DEFAULT_IMAGE.into(),
            cpus: 1.0,
            mem_mib: 512,
            timeout_secs: 60,
            network: false,
            allowed_hosts: Vec::new(),
        }
    }

    /// Caps applied by the host regardless of what a peer asks for.
    #[must_use]
    pub fn clamp(mut self, max: &Limits) -> Self {
        self.cpus = self.cpus.clamp(0.1, max.cpus);
        self.mem_mib = self.mem_mib.clamp(16, max.mem_mib);
        self.timeout_secs = self.timeout_secs.clamp(1, max.timeout_secs);
        self.network = self.network && max.network;
        self.allowed_hosts.retain(|h| max.allowed_hosts.contains(h));
        self
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub cpus: f32,
    pub mem_mib: u32,
    pub timeout_secs: u64,
    pub network: bool,
    /// Destinations a task may be limited to, for runners that can enforce a host list.
    pub allowed_hosts: Vec<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cpus: 2.0,
            mem_mib: 1024,
            timeout_secs: 300,
            network: false,
            allowed_hosts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskOutput {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// blake3 of the exact sandbox policy the task ran under, for runners that have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_digest: Option<String>,
}

/// A read-only host directory shown to a task at `container`. Only the host itself builds these (never a
/// peer's `TaskSpec`), so a member cannot name a host path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub host: std::path::PathBuf,
    pub container: String,
}

impl Mount {
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn check(&self) -> Result<()> {
        let h = self.host.to_str().context("mount path is not utf-8")?;
        if !self.host.is_absolute() || h.contains([',', '\n', '\r', '\0']) {
            bail!("mount source must be an absolute path without commas or newlines");
        }
        if !self.container.starts_with('/')
            || self.container.contains([',', '\n', '\r', '\0', ':'])
            || self.container.split('/').any(|p| p == "..")
        {
            bail!("mount target must be an absolute path without commas, colons or `..`");
        }
        if matches!(self.container.as_str(), "/" | "/workspace" | "/tmp") {
            bail!("mount target {:?} is reserved", self.container);
        }
        Ok(())
    }
}

#[async_trait]
pub trait Runner: Send + Sync {
    fn name(&self) -> &'static str;
    /// The isolation this backend provides when it works. Backends must not overstate it.
    fn isolation(&self) -> Isolation;
    /// Check the backend can actually run tasks (daemon up, KVM present...). Called when a host
    /// starts; an error stops the host rather than letting tasks fail or fall back later.
    async fn preflight(&self) -> Result<()> {
        Ok(())
    }
    /// Whether tasks may be limited to a host list (`TaskSpec::allowed_hosts`).
    fn enforces_host_list(&self) -> bool {
        false
    }
    /// Facts about the sandbox technology (version, base-policy digest) for the host's signed statement.
    fn attestation(&self) -> String {
        String::new()
    }
    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput>;
    /// Like [`Runner::run`], with read-only host directories mounted into the task. Runners that cannot
    /// provide them refuse rather than run without.
    async fn run_with_mounts(
        &self,
        spec: &TaskSpec,
        workspace: &Path,
        mounts: &[Mount],
    ) -> Result<TaskOutput> {
        if !mounts.is_empty() {
            bail!(
                "the {} runner cannot mount host directories into a task",
                self.name()
            );
        }
        self.run(spec, workspace).await
    }
    /// The content digest of the image a task would run (for receipts). Runners without images refuse.
    async fn image_digest(&self, image: &str) -> Result<[u8; 32]> {
        let _ = image;
        bail!("the {} runner cannot report an image digest", self.name())
    }
}

/// Pick a backend by name. The one place a new sandbox technology is registered.
/// `process` (no isolation) needs `PLAYBOX_ALLOW_UNSAFE_RUNNER`.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn runner_by_name(name: &str, policy: SandboxPolicy) -> Result<std::sync::Arc<dyn Runner>> {
    match name {
        "docker" => Ok(std::sync::Arc::new(DockerRunner::new(policy))),
        "libkrun" | "krun" => Ok(std::sync::Arc::new(KrunRunner::locate(policy)?)),
        "openshell" => Ok(std::sync::Arc::new(OpenShellRunner::new(policy))),
        "process" if std::env::var_os("PLAYBOX_ALLOW_UNSAFE_RUNNER").is_some() => {
            Ok(std::sync::Arc::new(ProcessRunner))
        }
        "process" => {
            bail!("the process runner has no isolation; set PLAYBOX_ALLOW_UNSAFE_RUNNER to use it")
        }
        o => bail!("unknown runner {o:?}; use docker, libkrun, openshell or process"),
    }
}

/// How much a task may write while it runs. Measured as *growth* of its directory since it started
/// (so a large checkout does not count against it), in apparent bytes and in directory entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub max_bytes: u64,
    pub max_entries: usize,
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024 * 1024,
            max_entries: 100_000,
        }
    }
}

/// (apparent bytes of regular files, number of entries) under `dir`, not following symlinks.
/// Stops early once either limit is passed, so measuring a runaway directory stays cheap.
fn measure(dir: &Path, stop_bytes: u64, stop_entries: usize) -> (u64, usize) {
    let (mut bytes, mut entries) = (0u64, 0usize);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            entries += 1;
            let Ok(meta) = e.metadata() else { continue }; // does not follow symlinks
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.is_file() {
                bytes = bytes.saturating_add(meta.len());
            }
            if bytes > stop_bytes || entries > stop_entries {
                return (bytes, entries);
            }
        }
    }
    (bytes, entries)
}

/// Resolves with an error once `dir` has grown past `quota` since this was called. Never resolves
/// otherwise; race it against a task and drop the task when it fires.
pub async fn quota_exceeded(dir: &Path, quota: Quota) -> anyhow::Error {
    let probe = |stop_b, stop_e| {
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || measure(&dir, stop_b, stop_e))
    };
    let (base_bytes, base_entries) = probe(u64::MAX, usize::MAX).await.unwrap_or_default();
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (b, n) = probe(
            base_bytes.saturating_add(quota.max_bytes),
            base_entries.saturating_add(quota.max_entries),
        )
        .await
        .unwrap_or_default();
        if b.saturating_sub(base_bytes) > quota.max_bytes {
            return anyhow!(
                "task stopped: it wrote more than the host's scratch quota ({} MiB)",
                quota.max_bytes >> 20
            );
        }
        if n.saturating_sub(base_entries) > quota.max_entries {
            return anyhow!(
                "task stopped: it created more than the host's limit of {} files and directories",
                quota.max_entries
            );
        }
    }
}

/// Run `spec` with `runner` in `dir`, and stop it if it writes past `quota`. Stopping drops the run, which
/// kills the sandbox process (and, for Docker, the container).
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub async fn run_with_quota(
    runner: &dyn Runner,
    spec: &TaskSpec,
    dir: &Path,
    quota: Quota,
) -> Result<TaskOutput> {
    tokio::select! {
        out = runner.run(spec, dir) => out,
        why = quota_exceeded(dir, quota) => Err(why),
    }
}

fn check_spec(spec: &TaskSpec) -> Result<()> {
    if spec.cmd.is_empty() {
        bail!("empty command");
    }
    if !spec.allowed_hosts.is_empty() {
        bail!("this runner cannot limit a task to specific hosts; use the openshell runner");
    }
    if spec.image.starts_with('-') || spec.image.contains(char::is_whitespace) {
        bail!("invalid image name");
    }
    Ok(())
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(mut r: R) -> String {
    let mut buf = Vec::new();
    let _ = (&mut r).take(MAX_OUTPUT as u64).read_to_end(&mut buf).await;
    // Keep draining so the child never blocks on a full pipe.
    let _ = tokio::io::copy(&mut r, &mut tokio::io::sink()).await;
    String::from_utf8_lossy(&buf).into_owned()
}

async fn run_child(mut cmd: Command, timeout: Duration) -> Result<(TaskOutput, bool)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn")?;
    let out = tokio::spawn(read_capped(child.stdout.take().expect("piped")));
    let err = tokio::spawn(read_capped(child.stderr.take().expect("piped")));
    if let Ok(status) = tokio::time::timeout(timeout, child.wait()).await {
        let status = status?;
        Ok((
            TaskOutput {
                exit_code: status.code(),
                timed_out: false,
                stdout: out.await.unwrap_or_default(),
                stderr: err.await.unwrap_or_default(),
                policy_digest: None,
            },
            false,
        ))
    } else {
        let _ = child.kill().await;
        Ok((
            TaskOutput {
                exit_code: None,
                timed_out: true,
                stdout: out.await.unwrap_or_default(),
                stderr: err.await.unwrap_or_default(),
                policy_digest: None,
            },
            true,
        ))
    }
}

#[derive(Default)]
pub struct DockerRunner {
    policy: SandboxPolicy,
}

impl DockerRunner {
    #[must_use]
    pub fn new(policy: SandboxPolicy) -> Self {
        Self { policy }
    }
}

#[async_trait]
impl Runner for DockerRunner {
    fn attestation(&self) -> String {
        self.policy.describe()
    }

    fn name(&self) -> &'static str {
        "docker"
    }

    fn isolation(&self) -> Isolation {
        Isolation::Container
    }

    async fn preflight(&self) -> Result<()> {
        let out = Command::new("docker")
            .args(["version", "--format", "{{.Server.Version}}"])
            .stdin(Stdio::null())
            .output()
            .await
            .context("docker is not installed")?;
        if !out.status.success() {
            bail!(
                "docker daemon is not reachable: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        // The limits a grant promises must really be enforced. Docker only warns and carries on when the
        // kernel lacks a controller (Raspberry Pi OS boots with `cgroup_disable=memory`), which would let a
        // task use all the host's memory while its grant says 1 GiB. Fail closed instead.
        let info = Command::new("docker")
            .args([
                "info",
                "--format",
                "{{.MemoryLimit}} {{.PidsLimit}} {{.CPUCfsQuota}}",
            ])
            .stdin(Stdio::null())
            .output()
            .await
            .context("docker info")?;
        let missing = missing_limits(&String::from_utf8_lossy(&info.stdout));
        if !missing.is_empty() && std::env::var_os("PLAYBOX_ALLOW_MISSING_LIMITS").is_none() {
            bail!(
                "docker cannot enforce {} on this host, so a task could exceed what its grant allows. \
                 On a Raspberry Pi add `cgroup_enable=memory cgroup_memory=1` to /boot/firmware/cmdline.txt and reboot; \
                 otherwise use the libkrun runner. To run anyway, set PLAYBOX_ALLOW_MISSING_LIMITS=1.",
                missing.join(" and ")
            );
        }
        Ok(())
    }

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        self.run_with_mounts(spec, workspace, &[]).await
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
            .context("docker image inspect")?;
        if !out.status.success() {
            bail!(
                "docker cannot inspect image {image:?}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        parse_image_id(String::from_utf8_lossy(&out.stdout).trim())
    }

    async fn run_with_mounts(
        &self,
        spec: &TaskSpec,
        workspace: &Path,
        mounts: &[Mount],
    ) -> Result<TaskOutput> {
        check_spec(spec)?;
        self.policy.check(spec)?;
        for m in mounts {
            m.check()?;
        }
        let meta = tokio::fs::metadata(workspace).await.context("workspace")?;
        let mut rnd = [0u8; 6];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut rnd)?;
        let name = format!(
            "playbox-{}",
            rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let ws = workspace.to_str().context("workspace path is not utf-8")?;

        let mut cmd = Command::new("docker");
        cmd.args(["run", "--rm", "--name", &name]);
        cmd.args(["--network", if spec.network { "bridge" } else { "none" }]);
        cmd.args([
            "--cpus",
            &spec.cpus.to_string(),
            "--memory",
            &format!("{}m", spec.mem_mib),
        ]);
        cmd.args([
            "--pids-limit",
            "256",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
        ]);
        cmd.args([
            "--read-only",
            "--tmpfs",
            "/tmp:rw,nosuid,size=256m",
            "--ulimit",
            "nofile=1024",
            "--ipc",
            "private",
        ]);
        cmd.args(["--user", &format!("{}:{}", meta.uid(), meta.gid())]);
        cmd.args([
            "-v",
            &format!("{ws}:/workspace"),
            "-w",
            "/workspace",
            "-e",
            "HOME=/tmp",
        ]);
        for m in mounts {
            cmd.args([
                "--mount",
                &format!(
                    "type=bind,source={},target={},readonly",
                    m.host.display(),
                    m.container
                ),
            ]);
        }
        cmd.arg(&spec.image).args(&spec.cmd);

        // If this future is dropped (a quota stop, a cancelled request) the `docker run` client dies but the
        // container would not; the guard kills it.
        let _guard = KillContainer(name.clone());
        let (out, timed_out) = run_child(cmd, Duration::from_secs(spec.timeout_secs)).await?;
        if timed_out {
            let _ = Command::new("docker").args(["kill", &name]).output().await;
        }
        Ok(out)
    }
}

/// Kills a Docker container when dropped. After a normal exit the container is already gone (`--rm`) and
/// the kill is a harmless no-op.
struct KillContainer(String);

impl Drop for KillContainer {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker")
            .args(["kill", &self.0])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Runs the command directly on the host. No isolation at all.
pub struct ProcessRunner;

#[async_trait]
impl Runner for ProcessRunner {
    fn name(&self) -> &'static str {
        "process-UNSAFE"
    }

    fn isolation(&self) -> Isolation {
        Isolation::None
    }

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        check_spec(spec)?;
        let mut cmd = Command::new(&spec.cmd[0]);
        cmd.args(&spec.cmd[1..]).current_dir(workspace);
        Ok(run_child(cmd, Duration::from_secs(spec.timeout_secs))
            .await?
            .0)
    }
}

/// `sha256:<64 hex>` as printed by `docker image inspect --format {{.Id}}`.
fn parse_image_id(s: &str) -> Result<[u8; 32]> {
    let hex = s
        .strip_prefix("sha256:")
        .context("image id is not a sha256 digest")?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("image id is not a sha256 digest");
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

/// Which resource limits `docker info` says are unavailable. `info` is "<memory> <pids> <cpu quota>" as
/// `true`/`false`; anything unrecognised counts as unavailable, so a format change cannot silently pass.
fn missing_limits(info: &str) -> Vec<&'static str> {
    let flags: Vec<&str> = info.split_whitespace().collect();
    let on = |i: usize| flags.get(i) == Some(&"true");
    let mut missing = Vec::new();
    if !on(0) {
        missing.push("memory limits");
    }
    if !on(1) {
        missing.push("process limits");
    }
    if !on(2) {
        missing.push("CPU limits");
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_is_ordered() {
        assert!(
            Isolation::None < Isolation::Container && Isolation::Container < Isolation::MicroVm
        );
    }

    #[test]
    fn policy_refuses_unlisted_images() {
        let p = SandboxPolicy::default();
        assert!(p.check(&TaskSpec::new(&["true"])).is_ok());
        let mut spec = TaskSpec::new(&["true"]);
        spec.image = "evil/miner:latest".into();
        assert!(p.check(&spec).is_err());
    }

    #[test]
    fn mounts_are_checked_and_unsupported_runners_refuse_them() {
        let ok = Mount {
            host: "/srv/weights".into(),
            container: "/weights".into(),
        };
        assert!(ok.check().is_ok());
        for bad in [
            Mount {
                host: "relative/path".into(),
                container: "/weights".into(),
            },
            Mount {
                host: "/srv/a,b".into(),
                container: "/weights".into(),
            },
            Mount {
                host: "/srv/w".into(),
                container: "weights".into(),
            },
            Mount {
                host: "/srv/w".into(),
                container: "/a/../etc".into(),
            },
            Mount {
                host: "/srv/w".into(),
                container: "/w:rw".into(),
            },
            Mount {
                host: "/srv/w".into(),
                container: "/workspace".into(),
            },
            Mount {
                host: "/srv/w".into(),
                container: "/".into(),
            },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let r = rt.block_on(ProcessRunner.run_with_mounts(
            &TaskSpec::new(&["true"]),
            Path::new("/tmp"),
            &[ok],
        ));
        assert!(r.unwrap_err().to_string().contains("cannot mount"));
        assert!(rt.block_on(ProcessRunner.image_digest("x")).is_err());
    }

    #[test]
    fn image_ids_parse_strictly() {
        let id = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(parse_image_id(&id).unwrap(), [0xab; 32]);
        for bad in [
            "",
            "sha256:",
            "sha256:zz",
            &format!("md5:{}", "ab".repeat(32)),
            &format!("sha256:{}", "ab".repeat(31)),
            &format!("sha256:{}", "AB".repeat(33)),
        ] {
            assert!(parse_image_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn unsafe_runner_needs_opt_in() {
        std::env::remove_var("PLAYBOX_ALLOW_UNSAFE_RUNNER");
        assert!(runner_by_name("process", SandboxPolicy::default()).is_err());
        assert!(runner_by_name("nope", SandboxPolicy::default()).is_err());
        assert_eq!(
            runner_by_name("docker", SandboxPolicy::default())
                .unwrap()
                .isolation(),
            Isolation::Container
        );
    }

    #[test]
    fn missing_docker_limits_are_detected_and_unknown_output_fails_closed() {
        assert!(missing_limits("true true true").is_empty());
        assert_eq!(missing_limits("false true true"), ["memory limits"]);
        assert_eq!(
            missing_limits("true false false"),
            ["process limits", "CPU limits"]
        );
        // Garbage, empty output, or a missing field all count as unavailable.
        assert_eq!(missing_limits("").len(), 3);
        // `<no value>` shifts the fields, which is still a refusal rather than a pass.
        assert!(!missing_limits("<no value> true true").is_empty());
        assert_eq!(missing_limits("true true"), ["CPU limits"]);
    }
}
