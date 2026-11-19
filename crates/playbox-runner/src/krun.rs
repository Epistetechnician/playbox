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

#[cfg(target_os = "linux")]
use super::{cgroup::Limits, confine::Confinement, netpolicy::Level};
use super::{
    check_spec, egress::EgressConfig, guestnames, quota_exceeded, run_child_with_stdin,
    secrets::SecretStore, Isolation, Quota, Runner, SandboxPolicy, TaskOutput, TaskSpec,
};
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
    /// Named model-API upstreams proxied on host loopback (`BOX-7`).
    egress: Option<EgressConfig>,
    /// Linux confinement of the helper and the task's network; `None` runs the helper as before.
    #[cfg(target_os = "linux")]
    confine: Option<Confinement>,
}

/// Why a task's network request must be refused, if it must.
///
/// libkrun's TSI hands the guest the host's own network access, so a guest with a network reaches the host's loopback,
/// LAN and cloud metadata service. Only the Linux confinement filters that. Everywhere else the request is refused unless
/// the host owner opts in with `PLAYBOX_ALLOW_UNFILTERED_NETWORK`.
fn unfiltered_network_refusal(
    wants_network: bool,
    filtered: bool,
    owner_opted_in: bool,
) -> Option<&'static str> {
    (wants_network && !filtered && !owner_opted_in).then_some(
        "this libkrun host cannot filter a guest's network (TSI reaches the host's loopback, LAN and metadata service); \
         run tasks without a network, use a confined Linux host, or set PLAYBOX_ALLOW_UNFILTERED_NETWORK=1 on the host",
    )
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
            secrets: SecretStore::from_env(),
            egress: None,
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

    /// Configure host-side model API egress upstreams (`BOX-7`).
    #[must_use]
    pub fn with_egress(mut self, cfg: EgressConfig) -> Self {
        self.egress = Some(cfg);
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
            return format!(
                "{};{};helper={}",
                self.policy.describe(),
                c.describe(),
                helper_digest(&self.helper)
            );
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
        if self.egress.is_some() {
            plain.egress.clear();
        }
        #[cfg(target_os = "linux")]
        if self.confine.is_some() {
            plain.allowed_hosts.clear(); // enforced below by the network filter
        }
        check_spec(&plain)?;
        self.policy.check(spec)?;
        #[cfg(target_os = "linux")]
        let filtered = self.confine.is_some();
        #[cfg(not(target_os = "linux"))]
        let filtered = false;
        let wants_tsi = spec.network || !spec.egress.is_empty();
        if let Some(why) = unfiltered_network_refusal(
            wants_tsi,
            filtered,
            crate::env_opt_in("PLAYBOX_ALLOW_UNFILTERED_NETWORK"),
        ) {
            bail!(why);
        }
        let egress_proxy = if spec.egress.is_empty() {
            None
        } else {
            let cfg = self
                .egress
                .as_ref()
                .ok_or_else(|| anyhow!("this host has no egress upstreams configured"))?;
            Some(super::egress::Proxy::start(cfg, &self.secrets, &spec.egress).await?)
        };
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
        let env_stdin = egress_proxy
            .as_ref()
            .and_then(|proxy| env_stdin_payload(proxy.env_vars()));
        if env_stdin.is_some() {
            // Pass capability-bearing egress URLs over stdin rather than --env argv so host /proc/cmdline never sees the token.
            cmd.arg("--env-stdin");
        }
        #[cfg(target_os = "linux")]
        let egress_port = egress_proxy.as_ref().map(super::egress::Proxy::port);
        // Held until the task ends: dropping it kills the cgroup and removes the network chain.
        #[cfg(target_os = "linux")]
        let _guard = match &self.confine {
            Some(c) => match self
                .confine_args(
                    c,
                    spec,
                    &clone,
                    workspace,
                    staged.path(),
                    vcpus,
                    mem,
                    egress_port,
                    &mut cmd,
                )
                .await
            {
                Ok(g) => Some(g),
                Err(e) => {
                    let _ = tokio::fs::remove_dir_all(&clone).await;
                    return Err(e);
                }
            },
            None => {
                if wants_tsi {
                    cmd.arg("--network");
                    if spec.network {
                        // Unfiltered (the owner opted in above): the guest shares the host's network, so its resolvers work as they are.
                        give_resolvers(&clone, |_| true);
                    }
                }
                None
            }
        };
        #[cfg(not(target_os = "linux"))]
        if wants_tsi {
            cmd.arg("--network");
            if spec.network {
                give_resolvers(&clone, |_| true);
            }
        }
        cmd.arg("--").args(&spec.cmd);

        // The guest's root is a clone on the host's disk, so a guest writing outside /workspace fills the host.
        // Watch it; stopping the race kills the helper and so the VM.
        let result = tokio::select! {
            r = run_child_with_stdin(cmd, Duration::from_secs(spec.timeout_secs), env_stdin) => r,
            why = quota_exceeded(&clone, self.quota) => Err(why),
        };
        let _ = tokio::fs::remove_dir_all(&clone).await;
        let _ = tokio::fs::remove_dir_all(clone.with_extension("jail")).await;
        let mut out = result
            .with_context(|| format!("running {}", self.helper.display()))?
            .0;
        out.secrets_used = staged.used.clone();
        if let Some(proxy) = egress_proxy {
            out.secrets_used.extend_from_slice(proxy.secrets_used());
        }
        Ok(out)
    }
}

#[cfg(target_os = "linux")]
fn helper_digest(helper: &Path) -> String {
    std::fs::read(helper).map_or_else(
        |_| "unreadable".into(),
        |b| blake3::hash(&b).to_hex().chars().take(16).collect(),
    )
}

#[cfg(target_os = "linux")]
impl KrunRunner {
    /// Create the task's cgroup and network chain and add the helper flags that make it confine itself.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call site; the values are all per-task"
    )]
    async fn confine_args(
        &self,
        c: &Confinement,
        spec: &TaskSpec,
        clone: &Path,
        workspace: &Path,
        secrets: &Path,
        vcpus: u32,
        mem: u32,
        egress_port: Option<u16>,
        cmd: &mut Command,
    ) -> Result<super::confine::RunGuard> {
        let (level, dests) = if spec.allowed_hosts.is_empty() {
            if spec.network {
                // An open network: resolvers the filter lets this task reach.
                give_resolvers(clone, |ip| super::netpolicy::allowed(Level::Internet, ip));
            }
            (
                if spec.network {
                    Level::Internet
                } else {
                    Level::None
                },
                Vec::new(),
            )
        } else {
            // A host list: the guest gets the pinned addresses by name and needs no resolver.
            let pinned = resolve_hosts(&spec.allowed_hosts).await?;
            let names: Vec<_> = pinned.iter().map(|(n, a)| (n.clone(), a.ip())).collect();
            if let Err(e) = guestnames::write_etc(clone, "hosts", &guestnames::hosts_file(&names)) {
                eprintln!("playbox: the guest gets no /etc/hosts: {e:#}");
            }
            (
                Level::Internet,
                pinned.into_iter().map(|(_, a)| a).collect(),
            )
        };
        let run_id = random_name()?;
        let limits = Limits {
            cpus: vcpus as f32,
            guest_memory_mib: mem,
        };
        let guard = {
            let (c, id, dests) = (c.clone(), run_id.clone(), dests);
            tokio::task::spawn_blocking(move || {
                c.begin_with_egress(&id, limits, level, &dests, egress_port)
            })
            .await??
        };
        let jail_dir = clone.with_extension("jail");
        tokio::fs::create_dir_all(&jail_dir).await?;
        cmd.arg("--cgroup")
            .arg(c.cgroup_of(&run_id))
            .arg("--jail-dir")
            .arg(&jail_dir);
        cmd.arg("--rw").arg(clone).arg("--rw").arg(workspace);
        if !spec.secrets.is_empty() {
            cmd.arg("--ro").arg(secrets);
        }
        if level > Level::None || egress_port.is_some() {
            cmd.arg("--network");
        }
        if c.seccomp {
            cmd.arg("--seccomp");
        }
        Ok(guard)
    }
}

/// Write the guest's `/etc/resolv.conf` (see [`guestnames`]). A guest without it still runs and reaches addresses, so a
/// failure here is reported and the task goes on.
fn give_resolvers(clone: &Path, may_reach: impl Fn(std::net::IpAddr) -> bool) {
    let owner = std::env::var("PLAYBOX_GUEST_DNS").ok();
    let written = guestnames::choose(owner.as_deref(), &guestnames::host_resolv_conf(), may_reach)
        .and_then(|r| guestnames::write_etc(clone, "resolv.conf", &guestnames::resolv_conf(&r)));
    if let Err(e) = written {
        eprintln!("playbox: the guest gets no resolver: {e:#}");
    }
}

/// Resolve `host` or `host:port` (port 443 by default) once, so the filter pins addresses and a later DNS answer cannot move them.
#[cfg(target_os = "linux")]
async fn resolve_hosts(hosts: &[String]) -> Result<Vec<(String, std::net::SocketAddr)>> {
    let mut out = Vec::new();
    for h in hosts {
        let (name, port) = match h.rsplit_once(':') {
            Some((n, p)) if !n.contains(':') => (
                n,
                p.parse::<u16>()
                    .with_context(|| format!("bad port in {h:?}"))?,
            ),
            _ => (h.as_str(), 443),
        };
        let found: Vec<_> = tokio::net::lookup_host((name, port))
            .await
            .with_context(|| format!("resolving {name}"))?
            .collect();
        if found.is_empty() {
            bail!("{name} did not resolve");
        }
        out.extend(found.into_iter().map(|a| (name.to_string(), a)));
    }
    Ok(out)
}

fn env_stdin_payload(vars: &[(String, String)]) -> Option<Vec<u8>> {
    if vars.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (k, v) in vars {
        out.push_str(k);
        out.push('=');
        out.push_str(v);
        out.push('\n');
    }
    Some(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::{env_stdin_payload, unfiltered_network_refusal as refusal, KrunRunner, SandboxPolicy};
    use std::path::PathBuf;

    // REQ: BOX-5
    #[test]
    fn an_unfiltered_network_is_refused_unless_the_owner_opts_in() {
        assert!(refusal(true, false, false).is_some());
        assert!(refusal(true, false, true).is_none());
        assert!(refusal(true, true, false).is_none());
        assert!(refusal(false, false, false).is_none());
    }

    #[test]
    fn secrets_store_honours_playbox_secrets_dir() {
        let dir = std::env::temp_dir().join(format!(
            "playbox-krun-secrets-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("API_KEY"), b"secret").unwrap();
        std::env::set_var("PLAYBOX_SECRETS_DIR", &dir);
        let runner = KrunRunner::with_helper(SandboxPolicy::default(), PathBuf::from("/bin/true"));
        std::env::remove_var("PLAYBOX_SECRETS_DIR");
        assert!(runner.secrets.has("API_KEY"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // REQ: BOX-7
    #[test]
    fn egress_env_vars_travel_on_stdin_not_cmdline() {
        let vars = vec![(
            "PLAYBOX_EGRESS_MODEL_URL".to_string(),
            "http://127.0.0.1:41234/t/secret_tok/model".to_string(),
        )];
        let payload = String::from_utf8(env_stdin_payload(&vars).unwrap()).unwrap();
        assert_eq!(
            payload,
            "PLAYBOX_EGRESS_MODEL_URL=http://127.0.0.1:41234/t/secret_tok/model\n"
        );
        assert!(env_stdin_payload(&[]).is_none());
    }
}
