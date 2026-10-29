//! Task runners. A runner executes one command against a workspace directory.
//!
//! - [`DockerRunner`]: container isolation (not a microVM). Usable today on macOS and Linux.
//! - [`KrunRunner`]: the libkrun microVM runner (needs the `playbox-krun-helper` binary; see `scripts/install-krun.sh`).
//! - [`ProcessRunner`]: runs on the host with NO isolation. Development only, off unless asked for.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{os::unix::fs::MetadataExt, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

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
        Self { allowed_images: vec![DEFAULT_IMAGE.into()] }
    }
}

impl SandboxPolicy {
    pub fn check(&self, spec: &TaskSpec) -> Result<()> {
        if !self.allowed_images.iter().any(|i| i == &spec.image) {
            bail!("image {:?} is not allowed on this host (allowed: {})", spec.image, self.allowed_images.join(", "));
        }
        Ok(())
    }
}

mod krun;
pub use krun::{find_helper, KrunRunner};

pub const DEFAULT_IMAGE: &str = "alpine:3";

const MAX_OUTPUT: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskSpec {
    pub cmd: Vec<String>,
    pub image: String,
    pub cpus: f32,
    pub mem_mib: u32,
    pub timeout_secs: u64,
    pub network: bool,
}

impl TaskSpec {
    pub fn new(cmd: &[&str]) -> Self {
        Self {
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            image: DEFAULT_IMAGE.into(),
            cpus: 1.0,
            mem_mib: 512,
            timeout_secs: 60,
            network: false,
        }
    }

    /// Caps applied by the host regardless of what a peer asks for.
    pub fn clamp(mut self, max: &Limits) -> Self {
        self.cpus = self.cpus.clamp(0.1, max.cpus);
        self.mem_mib = self.mem_mib.clamp(16, max.mem_mib);
        self.timeout_secs = self.timeout_secs.clamp(1, max.timeout_secs);
        self.network = self.network && max.network;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub cpus: f32,
    pub mem_mib: u32,
    pub timeout_secs: u64,
    pub network: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self { cpus: 2.0, mem_mib: 1024, timeout_secs: 300, network: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskOutput {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
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
    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput>;
}

/// Pick a backend by name. The one place a new sandbox technology is registered.
/// `process` (no isolation) needs `PLAYBOX_ALLOW_UNSAFE_RUNNER`.
pub fn runner_by_name(name: &str, policy: SandboxPolicy) -> Result<std::sync::Arc<dyn Runner>> {
    match name {
        "docker" => Ok(std::sync::Arc::new(DockerRunner::new(policy))),
        "libkrun" | "krun" => Ok(std::sync::Arc::new(KrunRunner::locate(policy)?)),
        "process" if std::env::var_os("PLAYBOX_ALLOW_UNSAFE_RUNNER").is_some() => Ok(std::sync::Arc::new(ProcessRunner)),
        "process" => bail!("the process runner has no isolation; set PLAYBOX_ALLOW_UNSAFE_RUNNER to use it"),
        o => bail!("unknown runner {o:?}; use docker, libkrun or process"),
    }
}

fn check_spec(spec: &TaskSpec) -> Result<()> {
    if spec.cmd.is_empty() {
        bail!("empty command");
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
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn")?;
    let out = tokio::spawn(read_capped(child.stdout.take().expect("piped")));
    let err = tokio::spawn(read_capped(child.stderr.take().expect("piped")));
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => {
            let status = status?;
            Ok((
                TaskOutput {
                    exit_code: status.code(),
                    timed_out: false,
                    stdout: out.await.unwrap_or_default(),
                    stderr: err.await.unwrap_or_default(),
                },
                false,
            ))
        }
        Err(_) => {
            let _ = child.kill().await;
            Ok((
                TaskOutput {
                    exit_code: None,
                    timed_out: true,
                    stdout: out.await.unwrap_or_default(),
                    stderr: err.await.unwrap_or_default(),
                },
                true,
            ))
        }
    }
}

#[derive(Default)]
pub struct DockerRunner {
    policy: SandboxPolicy,
}

impl DockerRunner {
    pub fn new(policy: SandboxPolicy) -> Self {
        Self { policy }
    }
}

#[async_trait]
impl Runner for DockerRunner {
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
            bail!("docker daemon is not reachable: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        // The limits a grant promises must really be enforced. Docker only warns and carries on when the
        // kernel lacks a controller (Raspberry Pi OS boots with `cgroup_disable=memory`), which would let a
        // task use all the host's memory while its grant says 1 GiB. Fail closed instead.
        let info = Command::new("docker")
            .args(["info", "--format", "{{.MemoryLimit}} {{.PidsLimit}} {{.CPUCfsQuota}}"])
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
        check_spec(spec)?;
        self.policy.check(spec)?;
        let meta = tokio::fs::metadata(workspace).await.context("workspace")?;
        let mut rnd = [0u8; 6];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut rnd)?;
        let name = format!("playbox-{}", rnd.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let ws = workspace.to_str().context("workspace path is not utf-8")?;

        let mut cmd = Command::new("docker");
        cmd.args(["run", "--rm", "--name", &name]);
        cmd.args(["--network", if spec.network { "bridge" } else { "none" }]);
        cmd.args(["--cpus", &spec.cpus.to_string(), "--memory", &format!("{}m", spec.mem_mib)]);
        cmd.args(["--pids-limit", "256", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"]);
        cmd.args(["--read-only", "--tmpfs", "/tmp:rw,nosuid,size=256m", "--ulimit", "nofile=1024", "--ipc", "private"]);
        cmd.args(["--user", &format!("{}:{}", meta.uid(), meta.gid())]);
        cmd.args(["-v", &format!("{ws}:/workspace"), "-w", "/workspace", "-e", "HOME=/tmp"]);
        cmd.arg(&spec.image).args(&spec.cmd);

        let (out, timed_out) = run_child(cmd, Duration::from_secs(spec.timeout_secs)).await?;
        if timed_out {
            let _ = Command::new("docker").args(["kill", &name]).output().await;
        }
        Ok(out)
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
        Ok(run_child(cmd, Duration::from_secs(spec.timeout_secs)).await?.0)
    }
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
        assert!(Isolation::None < Isolation::Container && Isolation::Container < Isolation::MicroVm);
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
    fn unsafe_runner_needs_opt_in() {
        std::env::remove_var("PLAYBOX_ALLOW_UNSAFE_RUNNER");
        assert!(runner_by_name("process", SandboxPolicy::default()).is_err());
        assert!(runner_by_name("nope", SandboxPolicy::default()).is_err());
        assert_eq!(runner_by_name("docker", SandboxPolicy::default()).unwrap().isolation(), Isolation::Container);
    }

    #[test]
    fn missing_docker_limits_are_detected_and_unknown_output_fails_closed() {
        assert!(missing_limits("true true true").is_empty());
        assert_eq!(missing_limits("false true true"), ["memory limits"]);
        assert_eq!(missing_limits("true false false"), ["process limits", "CPU limits"]);
        // Garbage, empty output, or a missing field all count as unavailable.
        assert_eq!(missing_limits("").len(), 3);
        // `<no value>` shifts the fields, which is still a refusal rather than a pass.
        assert!(!missing_limits("<no value> true true").is_empty());
        assert_eq!(missing_limits("true true"), ["CPU limits"]);
    }
}
