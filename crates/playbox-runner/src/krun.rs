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

use super::{
    check_spec, quota_exceeded, run_child, secrets::SecretStore, Isolation, Quota, Runner, SandboxPolicy,
    TaskOutput, TaskSpec,
};
#[cfg(target_os = "linux")]
use super::{cgroup::Limits, confine::Confinement, netpolicy::Level};
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
    /// The host's credentials a task may ask for by name.
    secrets: SecretStore,
    /// Linux confinement of the helper and the task's network; `None` runs the helper as before.
    #[cfg(target_os = "linux")]
    confine: Option<Confinement>,
}

fn playbox_home() -> PathBuf {
    std::env::var_os("PLAYBOX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".playbox")
        })
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
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn find_helper() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("PLAYBOX_KRUN_HELPER") {
        return Ok(PathBuf::from(p));
    }
    let mut tries: Vec<PathBuf> = vec![PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(".cargo/bin")
        .join(HELPER)];
    if let Some(path) = std::env::var_os("PATH") {
        tries.extend(std::env::split_paths(&path).map(|d| d.join(HELPER)));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        tries.push(dir.join(HELPER));
    }
    tries.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        anyhow!("{HELPER} not found; run scripts/install-krun.sh (or set PLAYBOX_KRUN_HELPER)")
    })
}

impl KrunRunner {
    /// A runner using the helper found by [`find_helper`].
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn locate(policy: SandboxPolicy) -> Result<Self> {
        Ok(Self::with_helper(policy, find_helper()?))
    }

    /// Limit how much a guest may write outside `/workspace`, where its private root lives on the host's disk.
    #[must_use]
    pub fn with_quota(mut self, quota: Quota) -> Self {
        self.quota = quota;
        self
    }

    #[must_use]
    pub fn with_helper(policy: SandboxPolicy, helper: PathBuf) -> Self {
        Self {
            policy,
            helper,
            root: playbox_home().join("krun"),
            quota: Quota::default(),
            export: Arc::default(),
            secrets: SecretStore::new(playbox_home().join("secrets")),
            #[cfg(target_os = "linux")]
            confine: None,
        }
    }

    /// Where the host keeps secrets tasks may ask for by name (default `$PLAYBOX_HOME/secrets`).
    #[must_use]
    pub fn with_secrets(mut self, store: SecretStore) -> Self {
        self.secrets = store;
        self
    }

    /// Run each task's helper in a jail, under a cgroup, behind a network filter (Linux; see [`Confinement`]).
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn with_confinement(mut self, c: Confinement) -> Self {
        self.confine = Some(c);
        self
    }

    async fn docker(args: &[&str]) -> Result<String> {
        let out = Command::new("docker")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .context("docker is not installed")?;
        if !out.status.success() {
            bail!(
                "docker {}: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The cached root filesystem for `image`, exporting it from Docker if needed.
    async fn rootfs(&self, image: &str) -> Result<PathBuf> {
        let _one_at_a_time = self.export.lock().await;
        let id = if let Ok(id) =
            Self::docker(&["image", "inspect", "--format", "{{.Id}}", image]).await
        {
            id
        } else {
            // The host policy already limited which images can be named, so pulling one is safe.
            Self::docker(&["pull", "--quiet", "--platform", platform(), image]).await?;
            Self::docker(&["image", "inspect", "--format", "{{.Id}}", image]).await?
        };
        let key: String = id
            .trim_start_matches("sha256:")
            .chars()
            .filter(char::is_ascii_hexdigit)
            .take(40)
            .collect();
        if key.len() < 12 {
            bail!("unexpected image id {id:?}");
        }
        let dir = self.root.join("rootfs").join(&key);
        if dir.is_dir() {
            return Ok(dir);
        }
        let tmp = self
            .root
            .join("rootfs")
            .join(format!(".tmp-{}", random_name()?));
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
                bail!(
                    "extracting {image}: {}",
                    String::from_utf8_lossy(&st.stderr).trim()
                );
            }
            for d in ["dev", "proc", "sys", "tmp", "workspace"] {
                tokio::fs::create_dir_all(tree.join(d)).await?;
            }
            let _ = tokio::fs::remove_file(&tar).await;
            tokio::fs::rename(&tree, &dir).await.or_else(|e| {
                if dir.is_dir() {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
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
        let out = cp
            .arg(rootfs)
            .arg(&dst)
            .stdin(Stdio::null())
            .output()
            .await?;
        if !out.status.success() {
            bail!(
                "cloning the root filesystem: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(dst)
    }
}

#[async_trait]
impl Runner for KrunRunner {
    fn attestation(&self) -> String {
        #[cfg(target_os = "linux")]
        if let Some(c) = &self.confine {
            return format!("{};{};helper={}", self.policy.describe(), c.describe(), helper_digest(&self.helper));
        }
        format!("{};confine=off", self.policy.describe())
    }

    fn enforces_host_list(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(c) = &self.confine {
            return c.max_net > Level::None;
        }
        false
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
        #[cfg(target_os = "linux")]
        if let Some(c) = &self.confine {
            let c = c.clone();
            tokio::task::spawn_blocking(move || c.preflight()).await??;
        }
        let image = self
            .policy
            .allowed_images
            .first()
            .ok_or_else(|| anyhow!("the sandbox policy allows no images"))?;
        let ws = self.root.join("preflight");
        tokio::fs::create_dir_all(&ws).await?;
        let spec = TaskSpec {
            image: image.clone(),
            timeout_secs: 60,
            mem_mib: 256,
            ..TaskSpec::new(&["true"])
        };
        let out = self
            .run(&spec, &ws)
            .await
            .context("libkrun smoke test could not run")?;
        if out.exit_code != Some(0) {
            bail!(
                "libkrun smoke test failed (exit {:?}{}): {}{}",
                out.exit_code,
                if out.timed_out { ", timed out" } else { "" },
                out.stderr.trim(),
                if cfg!(target_os = "macos") {
                    " — on macOS the helper must be signed with the hypervisor entitlement (scripts/install-krun.sh)"
                } else {
                    ""
                }
            );
        }
        Ok(())
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is bounded, or truncated on purpose; the value is clamped to be non-negative first"
    )]
    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        let mut plain = spec.clone();
        plain.secrets.clear();
        #[cfg(target_os = "linux")]
        if self.confine.is_some() {
            plain.allowed_hosts.clear(); // enforced below by the network filter
        }
        check_spec(&plain)?;
        self.policy.check(spec)?;
        let rootfs = self.rootfs(&spec.image).await?;
        let clone = self.clone_rootfs(&rootfs).await?;
        let staged = self.secrets.stage(&spec.secrets, &self.root.join("run"));
        let staged = match staged {
            Ok(s) => s,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&clone).await;
                return Err(e);
            }
        };

        let vcpus = (spec.cpus.ceil() as u32).clamp(1, MAX_VCPUS);
        let mem = spec.mem_mib.max(MIN_MEM_MIB);
        let mut cmd = Command::new(&self.helper);
        cmd.arg("--root")
            .arg(&clone)
            .arg("--workspace")
            .arg(workspace);
        cmd.args(["--cpus", &vcpus.to_string(), "--mem", &mem.to_string()]);
        if !spec.secrets.is_empty() {
            cmd.arg("--secrets").arg(staged.path());
        }
        // Held until the task ends: dropping it kills the cgroup and removes the network chain.
        #[cfg(target_os = "linux")]
        let _guard = match &self.confine {
            Some(c) => match self.confine_args(c, spec, &clone, workspace, staged.path(), vcpus, mem, &mut cmd).await {
                Ok(g) => Some(g),
                Err(e) => {
                    let _ = tokio::fs::remove_dir_all(&clone).await;
                    return Err(e);
                }
            },
            None => {
                if spec.network {
                    cmd.arg("--network");
                }
                None
            }
        };
        #[cfg(not(target_os = "linux"))]
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
        let _ = tokio::fs::remove_dir_all(clone.with_extension("jail")).await;
        let mut out = result
            .with_context(|| format!("running {}", self.helper.display()))?
            .0;
        out.secrets_used = staged.used.clone();
        Ok(out)
    }
}

#[cfg(target_os = "linux")]
fn helper_digest(helper: &Path) -> String {
    std::fs::read(helper).map_or_else(|_| "unreadable".into(), |b| blake3::hash(&b).to_hex().chars().take(16).collect())
}

#[cfg(target_os = "linux")]
impl KrunRunner {
    /// Create the task's cgroup and network chain and add the helper flags that make it confine itself.
    #[allow(clippy::too_many_arguments, reason = "one call site; the values are all per-task")]
    async fn confine_args(
        &self,
        c: &Confinement,
        spec: &TaskSpec,
        clone: &Path,
        workspace: &Path,
        secrets: &Path,
        vcpus: u32,
        mem: u32,
        cmd: &mut Command,
    ) -> Result<super::confine::RunGuard> {
        let (level, dests) = if spec.allowed_hosts.is_empty() {
            (if spec.network { Level::Internet } else { Level::None }, Vec::new())
        } else {
            (Level::Internet, resolve_hosts(&spec.allowed_hosts).await?)
        };
        let run_id = random_name()?;
        let limits = Limits { cpus: vcpus as f32, guest_memory_mib: mem };
        let guard = {
            let (c, id, dests) = (c.clone(), run_id.clone(), dests);
            tokio::task::spawn_blocking(move || c.begin(&id, limits, level, &dests)).await??
        };
        let jail_dir = clone.with_extension("jail");
        tokio::fs::create_dir_all(&jail_dir).await?;
        cmd.arg("--cgroup").arg(c.cgroup_of(&run_id)).arg("--jail-dir").arg(&jail_dir);
        cmd.arg("--rw").arg(clone).arg("--rw").arg(workspace);
        if !spec.secrets.is_empty() {
            cmd.arg("--ro").arg(secrets);
        }
        if level > Level::None {
            cmd.arg("--network");
        }
        if c.seccomp {
            cmd.arg("--seccomp");
        }
        Ok(guard)
    }
}

/// Resolve `host` or `host:port` (port 443 by default) once, so the filter pins addresses and a later DNS answer cannot move them.
#[cfg(target_os = "linux")]
async fn resolve_hosts(hosts: &[String]) -> Result<Vec<std::net::SocketAddr>> {
    let mut out = Vec::new();
    for h in hosts {
        let (name, port) = match h.rsplit_once(':') {
            Some((n, p)) if !n.contains(':') => (n, p.parse::<u16>().with_context(|| format!("bad port in {h:?}"))?),
            _ => (h.as_str(), 443),
        };
        let found: Vec<_> = tokio::net::lookup_host((name, port)).await.with_context(|| format!("resolving {name}"))?.collect();
        if found.is_empty() {
            bail!("{name} did not resolve");
        }
        out.extend(found);
    }
    Ok(out)
}
