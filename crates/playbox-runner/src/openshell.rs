//! The NVIDIA OpenShell runner: each task gets its own OpenShell sandbox, driven through the
//! `openshell` CLI (https://github.com/NVIDIA/OpenShell).
//!
//! How a task runs:
//! 1. A policy file is generated per task: Landlock filesystem rules (`/workspace` and `/tmp`
//!    writable, system paths read-only), non-root user, and no `network_policies`, so the
//!    sandbox's default-deny network applies.
//! 2. `openshell sandbox create` starts a sandbox from the (host-allowed) image with the workspace
//!    uploaded to `/workspace`; the policy is passed through `OPENSHELL_SANDBOX_POLICY`.
//! 3. `openshell sandbox exec` runs the command; its exit code is the task's.
//! 4. The workspace is downloaded back so the host can diff it, and the sandbox is deleted.
//!
//! Known gaps, deliberately not papered over:
//! - OpenShell policies carry no CPU, memory or timeout limits. The flags are passed, but its VM
//!   driver ignores them, so `preflight` fails closed unless `PLAYBOX_ALLOW_MISSING_LIMITS` is set
//!   (same rule as the Docker runner). Timeouts are enforced here by killing the CLI and deleting
//!   the sandbox.
//! - A task asking for network is refused: OpenShell allows network per host and binary, and a
//!   grant has no such list yet.
//! - Isolation is reported as `Container`, the weakest claim that holds across its drivers.
//!   Written against OpenShell's docs; not yet run against a real gateway.

use super::{check_spec, run_child, Isolation, Runner, SandboxPolicy, TaskOutput, TaskSpec};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

const BIN: &str = "openshell";
const SETUP_TIMEOUT: Duration = Duration::from_secs(180);

pub struct OpenShellRunner {
    policy: SandboxPolicy,
}

impl OpenShellRunner {
    pub fn new(policy: SandboxPolicy) -> Self {
        Self { policy }
    }
}

/// The OpenShell policy for one task. Pure, so it can be tested and hashed into a receipt.
pub fn policy_yaml() -> String {
    "version: 1\n\
     filesystem_policy:\n  include_workdir: false\n  read_only: [/usr, /lib, /bin, /sbin, /etc]\n  read_write: [/workspace, /tmp]\n\
     landlock:\n  compatibility: hard_requirement\n\
     process:\n  run_as_user: sandbox\n  run_as_group: sandbox\n"
        .into()
}

/// OpenShell takes Kubernetes-style quantities.
fn cpu_arg(cpus: f32) -> String {
    format!("{}m", (cpus * 1000.0).round() as u32)
}

fn mem_arg(mem_mib: u32) -> String {
    format!("{mem_mib}Mi")
}

fn random_name() -> Result<String> {
    let mut b = [0u8; 6];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    Ok(format!("playbox-{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>()))
}

async fn cli(args: &[&str], envs: &[(&str, &str)], timeout: Duration) -> Result<(TaskOutput, bool)> {
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

#[async_trait]
impl Runner for OpenShellRunner {
    fn name(&self) -> &'static str {
        "openshell"
    }

    fn isolation(&self) -> Isolation {
        Isolation::Container
    }

    async fn preflight(&self) -> Result<()> {
        let (out, _) = cli(&["--version"], &[], Duration::from_secs(10))
            .await
            .context("openshell is not installed (https://github.com/NVIDIA/OpenShell)")?;
        ok(&out, "--version")?;
        let (out, _) = cli(&["sandbox", "list"], &[], Duration::from_secs(20)).await?;
        if out.exit_code != Some(0) {
            bail!("openshell gateway is not reachable: {}", out.stderr.trim());
        }
        if std::env::var_os("PLAYBOX_ALLOW_MISSING_LIMITS").is_none() {
            bail!(
                "OpenShell policies have no CPU, memory or timeout limits and its VM driver ignores --cpu/--memory, \
                 so a task could exceed what its grant allows. Use a gateway driver that enforces them (e.g. Kubernetes) \
                 and set PLAYBOX_ALLOW_MISSING_LIMITS=1 to confirm, or use the docker or libkrun runner."
            );
        }
        Ok(())
    }

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        check_spec(spec)?;
        self.policy.check(spec)?;
        if spec.network {
            bail!("the openshell runner has no per-host allow-list in grants yet, so it refuses tasks that ask for network");
        }
        let ws = workspace.to_str().context("workspace path is not utf-8")?;
        let name = random_name()?;

        let dir = tempdir()?;
        let policy_path = dir.join("policy.yaml");
        tokio::fs::write(&policy_path, policy_yaml()).await?;
        let policy_path = policy_path.to_str().context("temp path")?.to_string();

        let result = self.run_in_sandbox(spec, ws, &name, &policy_path, &dir).await;
        // Always tear down, even on error or timeout; a leaked sandbox keeps the host's resources.
        let _ = cli(&["sandbox", "delete", &name], &[], Duration::from_secs(60)).await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        result
    }
}

impl OpenShellRunner {
    async fn run_in_sandbox(&self, spec: &TaskSpec, ws: &str, name: &str, policy: &str, tmp: &Path) -> Result<TaskOutput> {
        let upload = format!("{ws}:/workspace");
        let (cpu, mem) = (cpu_arg(spec.cpus), mem_arg(spec.mem_mib));
        let (out, timed_out) = cli(
            &[
                "sandbox", "create", "--name", name, "--from", &spec.image, "--cpu", &cpu, "--memory", &mem, "--upload", &upload,
            ],
            &[("OPENSHELL_SANDBOX_POLICY", policy)],
            SETUP_TIMEOUT,
        )
        .await?;
        if timed_out {
            bail!("openshell sandbox create timed out");
        }
        ok(&out, "sandbox create")?;

        let secs = spec.timeout_secs.to_string();
        let mut args = vec!["sandbox", "exec", "-n", name, "--no-tty", "--no-login-shell", "--timeout", &secs, "--"];
        args.extend(spec.cmd.iter().map(String::as_str));
        // A little slack so OpenShell's own timeout reports first; ours is the backstop.
        let (task, timed_out) = cli(&args, &[], Duration::from_secs(spec.timeout_secs + 10)).await?;

        // Bring the workspace back so the host can diff it. Skipped on timeout: the state is partial.
        if !timed_out {
            let dl = tmp.join("out");
            let (o, _) = cli(&["sandbox", "download", name, "/workspace", dl.to_str().context("temp path")?], &[], SETUP_TIMEOUT).await?;
            ok(&o, "sandbox download")?;
            // The CLI may nest the directory under its own name; accept either layout.
            let src = if dl.join("workspace").is_dir() { dl.join("workspace") } else { dl };
            let cp = Command::new("cp")
                .arg("-a")
                .arg(format!("{}/.", src.display()))
                .arg(ws)
                .stdin(Stdio::null())
                .output()
                .await?;
            if !cp.status.success() {
                bail!("copying the workspace back failed: {}", String::from_utf8_lossy(&cp.stderr).trim());
            }
        }
        Ok(task)
    }
}

fn tempdir() -> Result<std::path::PathBuf> {
    let p = std::env::temp_dir().join(random_name()?);
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_has_no_network_and_requires_landlock() {
        let p = policy_yaml();
        assert!(p.starts_with("version: 1"));
        assert!(!p.contains("network_policies"));
        assert!(p.contains("hard_requirement"));
        assert!(p.contains("run_as_user: sandbox"));
    }

    #[test]
    fn quantities_are_kubernetes_style() {
        assert_eq!(cpu_arg(0.5), "500m");
        assert_eq!(cpu_arg(2.0), "2000m");
        assert_eq!(mem_arg(512), "512Mi");
    }

    #[tokio::test]
    async fn refuses_network_and_unlisted_images() {
        let r = OpenShellRunner::new(SandboxPolicy::default());
        let mut spec = TaskSpec::new(&["true"]);
        spec.network = true;
        assert!(r.run(&spec, Path::new("/tmp")).await.is_err());
        let mut spec = TaskSpec::new(&["true"]);
        spec.image = "evil/miner".into();
        assert!(r.run(&spec, Path::new("/tmp")).await.is_err());
    }
}
