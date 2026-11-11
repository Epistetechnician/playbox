//! Resource limits for each run through a cgroup v2 of its own.
//!
//! A guest's memory and vCPUs are bounded by libkrun, but the processes behind it (the VMM and anything it forks) are host
//! processes. A run's cgroup caps their memory, CPU time and process count, so a run cannot exhaust its host, and kills them all
//! at teardown. Also what the network filter matches (`netpolicy`): traffic is judged by the cgroup of the process that sends it.
//!
//! Design from Peerspan's runtime (`crates/runtime/src/cgroup.rs`, Apache-2.0): a supervisor holds an exclusive lock on its run's
//! cgroup directory for as long as the run lasts, and the kernel releases it when the supervisor dies however it dies. A cgroup
//! whose lock is free was left by a supervisor that died before tearing it down; [`stale`] finds them.
//!
//! Needs a writable cgroup v2 hierarchy (root, or a delegated subtree). Not available on macOS.

use anyhow::{bail, Context, Result};
use rustix::fs::{flock, FlockOperation};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Memory allowed on top of the guest's own, for the VMM process.
pub const OVERHEAD_BYTES: u64 = 256 << 20;
/// Processes and threads allowed per run: vCPU and device threads.
pub const PIDS_MAX: u32 = 512;
const CPU_PERIOD_US: u64 = 100_000;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The limits of one run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub cpus: f32,
    pub guest_memory_mib: u32,
}

/// A run's cgroup, killed and removed with everything in it when dropped.
#[derive(Debug)]
pub struct RunCgroup {
    path: PathBuf,
    /// The cgroup's directory, locked exclusively while the run is live.
    _lock: File,
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Whether `root` is a cgroup v2 directory this process may create children in.
#[must_use]
pub fn usable(root: &Path) -> bool {
    root.join("cgroup.controllers").is_file() && fs::metadata(root).map(|m| !m.permissions().readonly()).unwrap_or(false)
}

/// Make `root` a cgroup this process can create runs in: create it, and enable the controllers a run needs on its parent so
/// `root` has them to hand on. Needs the parent to hold no processes of its own (cgroup v2's "no internal processes" rule), which
/// is how a delegated subtree or a systemd slice looks; a caller running inside a container's own cgroup moves itself into a
/// leaf first.
///
/// # Errors
///
/// Fails if the parent will not enable the controllers.
pub fn prepare_root(root: &Path) -> Result<()> {
    let parent = root.parent().context("a cgroup root needs a parent")?;
    fs::write(parent.join("cgroup.subtree_control"), "+cpu +memory +pids")
        .with_context(|| format!("enabling controllers on {}", parent.display()))?;
    fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))
}

impl RunCgroup {
    /// Create `root/name` with `limits`, lock it, and enable the controllers it needs on `root`.
    ///
    /// # Errors
    ///
    /// Fails if the hierarchy is not cgroup v2 or not writable, a controller is missing, or `name` is not a plain name.
    pub fn create(root: &Path, name: &str, limits: Limits) -> Result<Self> {
        if !valid_name(name) {
            bail!("bad cgroup name {name:?}");
        }
        if !(limits.cpus > 0.0 && limits.cpus <= 256.0) {
            bail!("bad cpu limit {}", limits.cpus);
        }
        fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
        let controllers = fs::read_to_string(root.join("cgroup.controllers")).context("not a cgroup v2 directory")?;
        let need = ["cpu", "memory", "pids"];
        if let Some(missing) = need.iter().find(|c| !controllers.split_whitespace().any(|x| x == **c)) {
            bail!("the {missing} controller is not available at {}", root.display());
        }
        fs::write(root.join("cgroup.subtree_control"), "+cpu +memory +pids").context("enabling controllers")?;
        let path = root.join(name);
        fs::create_dir(&path).with_context(|| format!("creating {}", path.display()))?;
        let lock = File::open(&path)?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).context("locking the cgroup")?;
        let me = Self { path, _lock: lock };
        let mem = u64::from(limits.guest_memory_mib) * (1 << 20) + OVERHEAD_BYTES;
        me.set("memory.max", &mem.to_string())?;
        me.set("memory.swap.max", "0")?;
        me.set("pids.max", &PIDS_MAX.to_string())?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "bounded above by 256 cpus")]
        let quota = (f64::from(limits.cpus) * CPU_PERIOD_US as f64) as u64;
        me.set("cpu.max", &format!("{quota} {CPU_PERIOD_US}"))?;
        Ok(me)
    }

    fn set(&self, file: &str, value: &str) -> Result<()> {
        fs::write(self.path.join(file), value).with_context(|| format!("writing {file}"))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Move process `pid` (and so its future children) into the cgroup.
    ///
    /// # Errors
    ///
    /// Fails if the kernel refuses the move.
    pub fn add(&self, pid: u32) -> Result<()> {
        self.set("cgroup.procs", &pid.to_string())
    }

    /// Whether any process is still in the cgroup.
    #[must_use]
    pub fn populated(&self) -> bool {
        fs::read_to_string(self.path.join("cgroup.procs")).map(|s| !s.trim().is_empty()).unwrap_or(false)
    }

    /// Kill every process in the cgroup and wait for them to leave.
    pub fn kill_all(&self) {
        let _ = self.set("cgroup.kill", "1");
        let until = Instant::now() + DRAIN_TIMEOUT;
        while self.populated() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for RunCgroup {
    fn drop(&mut self) {
        self.kill_all();
        let _ = fs::remove_dir(&self.path);
    }
}

/// Run cgroups under `root` that no supervisor holds: left behind by one that died before tearing its run down. The caller
/// removes them (killing what is inside).
///
/// # Errors
///
/// Fails if `root` cannot be listed.
pub fn stale(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in fs::read_dir(root)? {
        let p = e?.path();
        if !p.is_dir() {
            continue;
        }
        let Ok(dir) = File::open(&p) else { continue };
        if flock(&dir, FlockOperation::NonBlockingLockExclusive).is_ok() {
            out.push(p);
        }
    }
    Ok(out)
}

/// Remove a stale cgroup: kill what is inside, then remove the directory.
///
/// # Errors
///
/// Fails if the directory cannot be removed after the processes are gone.
pub fn remove(path: &Path) -> Result<()> {
    let _ = fs::write(path.join("cgroup.kill"), "1");
    let until = Instant::now() + DRAIN_TIMEOUT;
    while fs::read_to_string(path.join("cgroup.procs")).map(|s| !s.trim().is_empty()).unwrap_or(false) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::remove_dir(path).with_context(|| format!("removing {}", path.display()))
}
