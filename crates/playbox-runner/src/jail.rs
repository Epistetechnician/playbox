//! Confines a helper process before it handles guest input.
//!
//! libkrun's devices act with the full authority of the process that runs them, so a bug in the VMM is a bug in a process that
//! can read the host user's files: the room's endpoint key, the identity and grants under `~/.playbox`, SSH keys, other
//! projects. [`Jail::enter`] moves the process into new mount, PID, IPC, UTS (and, unless the guest is meant to have a network,
//! network) namespaces and pivots into a root that holds only the paths listed, so a compromised VMM sees the task's own files
//! and nothing else, has no other process to attack, and cannot gain privileges.
//!
//! The mechanism follows Peerspan's runtime (`crates/runtime/src/jail.rs`, Apache-2.0), changed to work without root: an
//! unprivileged process first creates a user namespace that maps its own uid and gid to themselves, which gives it the right to
//! mount inside its own namespaces and no more.
//!
//! The caller must be single-threaded: `unshare` of a user namespace fails otherwise. Every failure is an error and the caller
//! must then exit; booting a VM unconfined after a failed jail would defeat the point.

use anyhow::{anyhow, bail, Context, Result};
use rustix::fs::{statvfs, StatVfsMountFlags};
use rustix::mount::{mount, mount_bind, mount_change, mount_remount, unmount, MountFlags, MountPropagationFlags, UnmountFlags};
use rustix::process::{getgid, getuid, pivot_root, set_dumpable_behavior, set_parent_process_death_signal, DumpableBehavior, Signal};
use rustix::thread::{set_no_new_privs, unshare_unsafe, UnshareFlags};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// What the jail holds and whether the process keeps a network.
#[derive(Debug, Clone, Default)]
pub struct Jail {
    /// An empty directory that becomes the new root's mount point. It stays empty on the host.
    pub dir: PathBuf,
    /// Paths bound read-only at the same path inside the jail (libraries the helper loads, the image's root filesystem).
    pub read_only: Vec<PathBuf>,
    /// Paths bound read-write (the task's workspace).
    pub read_write: Vec<PathBuf>,
    /// Device nodes bound in, such as `/dev/kvm`.
    pub devices: Vec<PathBuf>,
    /// Keep the host's network namespace. Without it the process has loopback only.
    pub network: bool,
}

fn flags_of(path: &Path) -> Result<MountFlags> {
    let v = statvfs(path).with_context(|| format!("statvfs {}", path.display()))?;
    let mut f = MountFlags::NOSUID | MountFlags::NODEV;
    for (have, set) in [
        (StatVfsMountFlags::NOEXEC, MountFlags::NOEXEC),
        (StatVfsMountFlags::NOATIME, MountFlags::NOATIME),
        (StatVfsMountFlags::NODIRATIME, MountFlags::NODIRATIME),
        (StatVfsMountFlags::RELATIME, MountFlags::RELATIME),
    ] {
        if v.f_flag.contains(have) {
            f |= set;
        }
    }
    Ok(f)
}

fn target_of(root: &Path, path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        bail!("jail paths must be absolute without `..`: {}", path.display());
    }
    Ok(root.join(path.strip_prefix("/").unwrap_or(path)))
}

impl Jail {
    fn bind(&self, root: &Path, src: &Path, read_only: bool) -> Result<()> {
        let dst = target_of(root, src)?;
        let meta = fs::metadata(src).with_context(|| format!("jail path {}", src.display()))?;
        if meta.is_dir() {
            fs::DirBuilder::new().recursive(true).mode(0o755).create(&dst)?;
        } else {
            if let Some(parent) = dst.parent() {
                fs::DirBuilder::new().recursive(true).mode(0o755).create(parent)?;
            }
            fs::File::create(&dst)?;
        }
        mount_bind(src, &dst).with_context(|| format!("bind {}", src.display()))?;
        if read_only {
            let flags = flags_of(src)? | MountFlags::BIND | MountFlags::RDONLY;
            mount_remount(&dst, flags, "").with_context(|| format!("remount {} read-only", src.display()))?;
        }
        Ok(())
    }

    /// Enter the jail. Afterwards the process sees only the listed paths and a fresh `/proc` of its own PID namespace, has no
    /// network (unless `network`), runs with the uid and gid it started with, and cannot gain privileges.
    ///
    /// Entering a PID namespace needs a new process, so this forks: the original process waits for the jailed one and exits with
    /// its status, and only the jailed one returns.
    ///
    /// # Errors
    ///
    /// Fails if any step fails; the process must then exit rather than run unconfined.
    pub fn enter(&self) -> Result<()> {
        let (uid, gid) = (getuid(), getgid());
        let rootless = !uid.is_root();
        set_no_new_privs(true).context("no_new_privs")?;

        let mut flags = UnshareFlags::NEWNS | UnshareFlags::NEWIPC | UnshareFlags::NEWUTS | UnshareFlags::NEWPID;
        if !self.network {
            flags |= UnshareFlags::NEWNET;
        }
        if rootless {
            flags |= UnshareFlags::NEWUSER;
        }
        // SAFETY: the caller is single-threaded (documented), and nothing else in the process relies on the namespaces
        // this replaces.
        unsafe { unshare_unsafe(flags) }.context("unshare (is the kernel allowing user namespaces?)")?;
        if rootless {
            fs::write("/proc/self/setgroups", "deny").context("setgroups")?;
            fs::write("/proc/self/uid_map", format!("{0} {0} 1\n", uid.as_raw())).context("uid_map")?;
            fs::write("/proc/self/gid_map", format!("{0} {0} 1\n", gid.as_raw())).context("gid_map")?;
        }
        // After the maps: a non-dumpable process's /proc/self files belong to root, which would have refused the writes above.
        set_dumpable_behavior(DumpableBehavior::NotDumpable).context("dumpable")?;

        // The original process keeps the write end open for as long as it lives; the jailed one reads EOF if it is gone. (Inside a
        // new PID namespace the parent has no PID to compare, so `getppid` cannot answer this.)
        let (alive_r, alive_w) = rustix::pipe::pipe().context("pipe")?;
        // SAFETY: fork in a single-threaded process; the child only makes ordinary calls.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            bail!("fork: {}", std::io::Error::last_os_error());
        }
        if pid > 0 {
            drop(alive_r);
            let _keep = alive_w;
            // The original process: report the jailed one's fate as its own.
            let mut status = 0;
            loop {
                // SAFETY: waitpid with a valid out-pointer.
                let r = unsafe { libc::waitpid(pid, &mut status, 0) };
                if r == pid {
                    break;
                }
                if r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    std::process::exit(125);
                }
            }
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                128 + libc::WTERMSIG(status)
            };
            std::process::exit(code);
        }

        // The jailed process: PID 1 of its namespace. If the original dies, so does this.
        drop(alive_w);
        set_parent_process_death_signal(Some(Signal::KILL)).context("pdeathsig")?;
        let mut fd = libc::pollfd { fd: std::os::fd::AsRawFd::as_raw_fd(&alive_r), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd; a zero timeout returns at once.
        let ready = unsafe { libc::poll(&mut fd, 1, 0) };
        if ready != 0 {
            bail!("the original process is already gone");
        }
        drop(alive_r);
        mount_change("/", MountPropagationFlags::REC | MountPropagationFlags::PRIVATE).context("make mounts private")?;
        let root = self.dir.as_path();
        mount("tmpfs", root, "tmpfs", MountFlags::NOSUID | MountFlags::NODEV, c"mode=0755,size=4m").context("tmpfs root")?;
        for p in &self.read_only {
            self.bind(root, p, true)?;
        }
        for p in &self.read_write {
            self.bind(root, p, false)?;
        }
        for p in &self.devices {
            self.bind(root, p, false)?;
        }
        let proc_dir = root.join("proc");
        fs::DirBuilder::new().mode(0o555).create(&proc_dir)?;
        mount("proc", &proc_dir, "proc", MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC, c"").context("mount /proc")?;
        let old = root.join(".old");
        fs::DirBuilder::new().mode(0o700).create(&old)?;
        pivot_root(root, &old).context("pivot_root")?;
        std::env::set_current_dir("/").context("chdir /")?;
        unmount("/.old", UnmountFlags::DETACH).context("unmount the old root")?;
        fs::remove_dir("/.old").context("remove /.old")?;
        fs::set_permissions("/", fs::Permissions::from_mode(0o555)).map_err(|e| anyhow!("chmod /: {e}"))?;
        Ok(())
    }
}
