//! The libkrun microVM runner: each task boots its own guest kernel behind the hypervisor
//! (Hypervisor.framework on Apple Silicon, KVM on Linux).
//!
//! How a task runs:
//! 1. The image (which the host policy must allow) is exported once from Docker into a cached root
//!    filesystem directory, keyed by image id so a moved tag gets a fresh one.
//! 2. Each task gets its own copy-on-write clone of that directory, so nothing a task writes outside
//!    `/workspace` survives, and the cache is never modified.
//! 3. A helper process (`playbox-krun-helper`, built with the `krun` feature and signed on macOS)
//!    boots the VM with the clone as `/` and the workspace shared at `/workspace`. It exits with the
//!    command's exit code; its stdout and stderr are the guest's.
//!
//! Network is off unless the task asks for it and the grant allows it. With it on, libkrun's TSI
//! gives the guest the host's network access *without* the public-address filtering the PRD wants,
//! so a guest can reach the host's loopback and LAN. Leave network off on hosts that matter.

use super::{check_spec, quota_exceeded, run_child, Isolation, Quota, Runner, SandboxPolicy, TaskOutput, TaskSpec};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::process::Command;

const HELPER: &str = "playbox-krun-helper";
const MIN_MEM_MIB: u32 = 128;
const MAX_VCPUS: u32 = 8;

pub struct KrunRunner {
    policy: SandboxPolicy,
    helper: PathBuf,
    root: PathBuf,
    /// How much a guest may write outside `/workspace` (its private root lives on the host's disk).
    quota: Quota,
    /// Only one rootfs export at a time per process.
    export: Arc<tokio::sync::Mutex<()>>,
}

fn playbox_home() -> PathBuf {
    std::env::var_os("PLAYBOX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".playbox"))
}

fn random_name() -> Result<String> {
    let mut b = [0u8; 8];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn platform() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "linux/arm64"
    } else {
        "linux/amd64"
    }
}

/// Where the helper lives: `$PLAYBOX_KRUN_HELPER`, `~/.cargo/bin`, PATH, then beside this executable.
///
/// Beside-the-executable comes last on purpose: a `cargo build` leaves an *unsigned* helper in
/// `target/`, and on macOS an unsigned helper cannot start a VM. The installed copy is signed by
/// `scripts/install-krun.sh`.
pub fn find_helper() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("PLAYBOX_KRUN_HELPER") {
        return Ok(PathBuf::from(p));
    }
    let mut tries: Vec<PathBuf> = vec![PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cargo/bin").join(HELPER)];
    if let Some(path) = std::env::var_os("PATH") {
        tries.extend(std::env::split_paths(&path).map(|d| d.join(HELPER)));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        tries.push(dir.join(HELPER));
    }
    tries
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("{HELPER} not found; run scripts/install-krun.sh (or set PLAYBOX_KRUN_HELPER)"))
}

impl KrunRunner {
    /// A runner using the helper found by [`find_helper`].
    pub fn locate(policy: SandboxPolicy) -> Result<Self> {
        Ok(Self::with_helper(policy, find_helper()?))
    }

    /// Limit how much a guest may write outside `/workspace`, where its private root lives on the host's disk.
    pub fn with_quota(mut self, quota: Quota) -> Self {
        self.quota = quota;
        self
    }

    pub fn with_helper(policy: SandboxPolicy, helper: PathBuf) -> Self {
        Self { policy, helper, root: playbox_home().join("krun"), quota: Quota::default(), export: Arc::default() }
    }

    async fn docker(args: &[&str]) -> Result<String> {
        let out = Command::new("docker").args(args).stdin(Stdio::null()).output().await.context("docker is not installed")?;
        if !out.status.success() {
            bail!("docker {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The cached root filesystem for `image`, exporting it from Docker if needed.
    async fn rootfs(&self, image: &str) -> Result<PathBuf> {
        let _one_at_a_time = self.export.lock().await;
        let id = match Self::docker(&["image", "inspect", "--format", "{{.Id}}", image]).await {
            Ok(id) => id,
            Err(_) => {
                // The host policy already limited which images can be named, so pulling one is safe.
                Self::docker(&["pull", "--quiet", "--platform", platform(), image]).await?;
                Self::docker(&["image", "inspect", "--format", "{{.Id}}", image]).await?
            }
        };
        let key: String = id.trim_start_matches("sha256:").chars().filter(char::is_ascii_hexdigit).take(40).collect();
        if key.len() < 12 {
            bail!("unexpected image id {id:?}");
        }
        let dir = self.root.join("rootfs").join(&key);
        if dir.is_dir() {
            return Ok(dir);
        }
        let tmp = self.root.join("rootfs").join(format!(".tmp-{}", random_name()?));
        tokio::fs::create_dir_all(&tmp).await?;
        let built = async {
            let cid = Self::docker(&["create", "--platform", platform(), image]).await?;
            let tar = tmp.join(".export.tar");
            let tar_s = tar.to_str().context("path is not utf-8")?;
            let exported = Self::docker(&["export", "-o", tar_s, &cid]).await;
            let _ = Self::docker(&["rm", "-f", &cid]).await;
            exported?;
            let tree = tmp.join("tree");
            tokio::fs::create_dir_all(&tree).await?;
            // Device nodes cannot be created by an ordinary user; the guest mounts its own /dev.
            let st = Command::new("tar")
                .args(["-xf", tar_s, "-C"])
                .arg(&tree)
                .args(["--exclude", "dev/*", "--exclude", "./dev/*"])
                .stdin(Stdio::null())
                .output()
                .await?;
            if !st.status.success() {
                bail!("extracting {image}: {}", String::from_utf8_lossy(&st.stderr).trim());
            }
            for d in ["dev", "proc", "sys", "tmp", "workspace"] {
                tokio::fs::create_dir_all(tree.join(d)).await?;
            }
            let _ = tokio::fs::remove_file(&tar).await;
            tokio::fs::rename(&tree, &dir).await.or_else(|e| if dir.is_dir() { Ok(()) } else { Err(e) })?;
            anyhow::Ok(())
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        built.with_context(|| format!("building the root filesystem for {image}"))?;
        Ok(dir)
    }

    /// A private copy-on-write clone of `rootfs` for one task.
    async fn clone_rootfs(&self, rootfs: &Path) -> Result<PathBuf> {
        let runs = self.root.join("run");
        tokio::fs::create_dir_all(&runs).await?;
        let dst = runs.join(random_name()?);
        let mut cp = Command::new("cp");
        if cfg!(target_os = "macos") {
            cp.arg("-c").arg("-R"); // APFS clonefile
        } else {
            cp.arg("-a").arg("--reflink=auto");
        }
        let out = cp.arg(rootfs).arg(&dst).stdin(Stdio::null()).output().await?;
        if !out.status.success() {
            bail!("cloning the root filesystem: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(dst)
    }
}

#[async_trait]
impl Runner for KrunRunner {
    fn attestation(&self) -> String {
        self.policy.describe()
    }

    fn name(&self) -> &'static str {
        "libkrun"
    }

    fn isolation(&self) -> Isolation {
        Isolation::MicroVm
    }

    /// Boots a real VM running `true`, so a missing library, firmware, entitlement or hypervisor
    /// fails when the host starts instead of on a member's first task.
    async fn preflight(&self) -> Result<()> {
        let image = self.policy.allowed_images.first().ok_or_else(|| anyhow!("the sandbox policy allows no images"))?;
        let ws = self.root.join("preflight");
        tokio::fs::create_dir_all(&ws).await?;
        let spec = TaskSpec { image: image.clone(), timeout_secs: 60, mem_mib: 256, ..TaskSpec::new(&["true"]) };
        let out = self.run(&spec, &ws).await.context("libkrun smoke test could not run")?;
        if out.exit_code != Some(0) {
            bail!(
                "libkrun smoke test failed (exit {:?}{}): {}{}",
                out.exit_code,
                if out.timed_out { ", timed out" } else { "" },
                out.stderr.trim(),
                if cfg!(target_os = "macos") { " — on macOS the helper must be signed with the hypervisor entitlement (scripts/install-krun.sh)" } else { "" }
            );
        }
        Ok(())
    }

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        check_spec(spec)?;
        self.policy.check(spec)?;
        let rootfs = self.rootfs(&spec.image).await?;
        let clone = self.clone_rootfs(&rootfs).await?;

        let vcpus = (spec.cpus.ceil() as u32).clamp(1, MAX_VCPUS);
        let mem = spec.mem_mib.max(MIN_MEM_MIB);
        let mut cmd = Command::new(&self.helper);
        cmd.arg("--root").arg(&clone).arg("--workspace").arg(workspace);
        cmd.args(["--cpus", &vcpus.to_string(), "--mem", &mem.to_string()]);
        if spec.network {
            cmd.arg("--network");
        }
        cmd.arg("--").args(&spec.cmd);

        // The guest's root is a clone on the host's disk, so a guest writing outside /workspace fills the host.
        // Watch it; stopping the race kills the helper and so the VM.
        let result = tokio::select! {
            r = run_child(cmd, Duration::from_secs(spec.timeout_secs)) => r,
            why = quota_exceeded(&clone, self.quota) => Err(why),
        };
        let _ = tokio::fs::remove_dir_all(&clone).await;
        Ok(result.with_context(|| format!("running {}", self.helper.display()))?.0)
    }
}
