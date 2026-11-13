//! Puts the pieces of Linux confinement together for the libkrun runner.
//!
//! * [`Confinement`] is the host owner's choice (`up --confine`): where run cgroups live, the most network a task may have, and
//!   whether the helper installs the syscall filter. It checks at host start that the kernel can do all of it, so a host never
//!   starts with confinement promised and absent.
//! * [`Confinement::begin`] runs in the supervisor for each task: it creates the task's cgroup with its memory, CPU and process
//!   limits, and loads the task's network chain. The returned [`RunGuard`] kills everything in the cgroup and removes the chain
//!   and the cgroup when dropped, however the task ended.
//! * [`Plan::enter`] runs in the helper process, before it creates any thread: join the cgroup, enter the jail, install the filter.
//!   A step that fails is an error and the helper exits; it never boots a VM unconfined.
//!
//! macOS has none of this and says so: its runner keeps the entitlement-signed helper and the notes in `docs/libkrun.md`.

use crate::cgroup::{self, Limits, RunCgroup};
use crate::jail::Jail;
use crate::netpolicy::{self, Level};
use crate::seccomp;
use anyhow::{bail, Context, Result};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The host owner's confinement settings.
#[derive(Debug, Clone)]
pub struct Confinement {
    /// The cgroup directory runs are created in, e.g. `/sys/fs/cgroup/playbox/runs`.
    pub cgroup_root: PathBuf,
    /// The most network any task may have. `Level::None` means the network filter is not needed and not loaded.
    pub max_net: Level,
    /// Install the syscall deny-list in the helper.
    pub seccomp: bool,
}

const CGROUP_MOUNT: &str = "/sys/fs/cgroup";

impl Confinement {
    /// Settings with the default cgroup root (`/sys/fs/cgroup/playbox/runs`).
    #[must_use]
    pub fn new(max_net: Level) -> Self {
        Self { cgroup_root: Path::new(CGROUP_MOUNT).join("playbox/runs"), max_net, seccomp: true }
    }

    /// The cgroup path relative to the cgroup mount, as the network rules name it.
    fn relative_root(&self) -> Result<String> {
        let rel = self.cgroup_root.strip_prefix(CGROUP_MOUNT).context("the cgroup root must be under /sys/fs/cgroup")?;
        Ok(rel.to_str().context("cgroup root is not utf-8")?.to_string())
    }

    /// For the host's signed statement: what a verifier can rely on about every task of this host.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "jail=userns+pidns+netns+pivot;cgroup=memory+cpu+pids;pids_max={};net_max={};{}",
            cgroup::PIDS_MAX,
            self.max_net,
            if self.seccomp { seccomp::describe() } else { "seccomp=off".to_string() }
        )
    }

    /// Check this machine can do everything promised, and load the fail-closed network base rules.
    ///
    /// # Errors
    ///
    /// Fails if user namespaces, the cgroup hierarchy or (when networking is allowed) nftables are unavailable.
    pub fn preflight(&self) -> Result<()> {
        let userns = std::fs::read_to_string("/proc/sys/user/max_user_namespaces").map(|s| s.trim() != "0").unwrap_or(true);
        if !userns {
            bail!("confinement needs user namespaces (user.max_user_namespaces is 0)");
        }
        let parent = self.cgroup_root.parent().context("cgroup root needs a parent")?;
        if !cgroup::usable(parent) {
            bail!("{} is not a writable cgroup v2 directory; run as root or with a delegated subtree", parent.display());
        }
        cgroup::prepare_root(&self.cgroup_root).context("preparing the run cgroup root")?;
        for stale in cgroup::stale(&self.cgroup_root).unwrap_or_default() {
            let _ = cgroup::remove(&stale);
        }
        if self.max_net > Level::None {
            // Start from nothing: chains left by a host that died are gone with the table.
            nft("add table inet playbox_runner\ndelete table inet playbox_runner\n")?;
            nft(&netpolicy::base_ruleset(&self.relative_root()?)?).context("loading the network base rules (is nftables installed, and are you root?)")?;
        }
        Ok(())
    }

    /// Create the limits for one task. `level` and `dests` are the task's network, at most `max_net`.
    ///
    /// # Errors
    ///
    /// Fails if the task asks for more network than the host allows, or the kernel refuses a limit or a rule. Nothing is left behind.
    pub fn begin(&self, run_id: &str, limits: Limits, level: Level, dests: &[SocketAddr]) -> Result<RunGuard> {
        if level > self.max_net {
            bail!("the task needs network level {level}, above this host's maximum {}", self.max_net);
        }
        let cg = RunCgroup::create(&self.cgroup_root, run_id, limits)?;
        let mut guard = RunGuard { cgroup: Some(cg), chain: None };
        if level > Level::None {
            let rules = netpolicy::run_chain(&self.relative_root()?, run_id, level, dests)?;
            nft(&rules).context("loading the task's network rules")?;
            guard.chain = Some(run_id.to_string());
        } else if self.max_net > Level::None {
            // A task with no network still gets its chain: an explicit reject, not the base table's default.
            nft(&netpolicy::run_chain(&self.relative_root()?, run_id, Level::None, &[])?)?;
            guard.chain = Some(run_id.to_string());
        }
        Ok(guard)
    }

    /// The cgroup directory of `run_id`, for the helper's `--cgroup`.
    #[must_use]
    pub fn cgroup_of(&self, run_id: &str) -> PathBuf {
        self.cgroup_root.join(run_id)
    }
}

/// A task's cgroup and network chain. Dropping it kills the task's processes and removes both.
#[derive(Debug)]
pub struct RunGuard {
    cgroup: Option<RunCgroup>,
    chain: Option<String>,
}

impl RunGuard {
    #[must_use]
    pub fn cgroup_path(&self) -> Option<&Path> {
        self.cgroup.as_ref().map(RunCgroup::path)
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Some(cg) = self.cgroup.take() {
            cg.kill_all();
            drop(cg);
        }
        if let Some(id) = self.chain.take() {
            let _ = remove_chain(&id);
        }
    }
}

fn nft(script: &str) -> Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run nft")?;
    child.stdin.take().context("nft stdin")?.write_all(script.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("nft: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Remove the jump rule (found by its handle), then the chain.
fn remove_chain(id: &str) -> Result<()> {
    let chain = format!("run_{id}");
    let listing = std::process::Command::new("nft").args(["-a", "list", "chain", "inet", "playbox_runner", "runs"]).output()?;
    let text = String::from_utf8_lossy(&listing.stdout);
    let needle = format!("jump {chain}");
    let handle = text
        .lines()
        .find(|l| l.contains(&needle))
        .and_then(|l| l.rsplit("# handle ").next())
        .and_then(|h| h.trim().parse::<u64>().ok());
    let mut script = String::new();
    if let Some(h) = handle {
        script.push_str(&format!("delete rule inet playbox_runner runs handle {h}\n"));
    }
    script.push_str(&format!("flush chain inet playbox_runner {chain}\ndelete chain inet playbox_runner {chain}\n"));
    nft(&script)
}

/// What the helper does to itself before it boots the VM.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// The run's cgroup directory to join.
    pub cgroup: Option<PathBuf>,
    pub jail: Option<Jail>,
    pub seccomp: bool,
}

impl Plan {
    /// Join the cgroup, enter the jail (which forks: only the jailed child returns), install the filter. Single-threaded callers only.
    ///
    /// # Errors
    ///
    /// Any failed step; the caller must exit.
    pub fn enter(&self) -> Result<()> {
        if let Some(cg) = &self.cgroup {
            std::fs::write(cg.join("cgroup.procs"), std::process::id().to_string()).context("joining the run's cgroup")?;
        }
        if let Some(j) = &self.jail {
            j.enter()?;
        }
        if self.seccomp {
            seccomp::install()?;
        }
        Ok(())
    }
}

/// Directories the helper needs inside the jail to load libkrun and libkrunfw: the parent of every library it has mapped, plus any
/// library directory holding a `libkrunfw` (loaded later, so not in the maps yet).
#[must_use]
pub fn library_dirs() -> Vec<PathBuf> {
    let mut dirs = std::collections::BTreeSet::new();
    if let Ok(maps) = std::fs::read_to_string("/proc/self/maps") {
        for line in maps.lines() {
            if let Some(path) = line.split_whitespace().nth(5).filter(|p| p.starts_with('/') && p.contains(".so")) {
                if let Some(parent) = Path::new(path).parent() {
                    dirs.insert(parent.to_path_buf());
                }
            }
        }
    }
    let mut search: Vec<PathBuf> = std::env::var("LD_LIBRARY_PATH").unwrap_or_default().split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    for d in ["/usr/lib", "/usr/lib64", "/usr/local/lib", "/usr/local/lib64", "/lib", "/lib64", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu"] {
        search.push(PathBuf::from(d));
    }
    for d in search {
        let has_fw = std::fs::read_dir(&d).map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with("libkrunfw"))).unwrap_or(false);
        if has_fw {
            dirs.insert(d);
        }
    }
    // A directory under another one in the list is already there.
    let all: Vec<PathBuf> = dirs.into_iter().collect();
    all.iter().filter(|d| !all.iter().any(|o| o != *d && d.starts_with(o))).cloned().collect()
}
