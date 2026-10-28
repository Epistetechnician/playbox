//! Task runners. A runner executes one command against a workspace directory.
//!
//! - [`DockerRunner`]: container isolation (not a microVM). Usable today on macOS and Linux.
//! - [`KrunRunner`]: the libkrun microVM runner. Not implemented yet; see PRD M2.
//! - [`ProcessRunner`]: runs on the host with NO isolation. Development only, off unless asked for.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{os::unix::fs::MetadataExt, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

pub mod workspace;

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
            image: "alpine:3".into(),
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
    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput>;
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

pub struct DockerRunner;

#[async_trait]
impl Runner for DockerRunner {
    fn name(&self) -> &'static str {
        "docker"
    }

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        check_spec(spec)?;
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

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        check_spec(spec)?;
        let mut cmd = Command::new(&spec.cmd[0]);
        cmd.args(&spec.cmd[1..]).current_dir(workspace);
        Ok(run_child(cmd, Duration::from_secs(spec.timeout_secs)).await?.0)
    }
}

/// Placeholder for the libkrun microVM runner (PRD M2). It refuses to run so nothing can
/// silently fall back to weaker isolation.
pub struct KrunRunner;

#[async_trait]
impl Runner for KrunRunner {
    fn name(&self) -> &'static str {
        "libkrun"
    }

    async fn run(&self, _spec: &TaskSpec, _workspace: &Path) -> Result<TaskOutput> {
        bail!("libkrun runner is not implemented yet (needs a Linux/KVM or macOS/HVF host with libkrun)")
    }
}
