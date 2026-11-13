//! Boots one libkrun microVM, runs a command in it, and exits with the command's exit code.
//!
//! libkrun's `krun_start_enter` never returns: it takes over the process and calls `exit()` when the
//! guest shuts down. So every task runs in its own short-lived helper process, started by
//! `KrunRunner`. On macOS this binary must be signed with the `com.apple.security.hypervisor`
//! entitlement (`scripts/install-krun.sh` does that).
//!
//! ```text
//! playbox-krun-helper --root DIR --workspace DIR --cpus N --mem MIB [--network] [--secrets DIR] -- CMD [ARG...]
//!     [--cgroup DIR] [--jail-dir DIR --rw DIR... --ro DIR...] [--seccomp]     (Linux confinement, see `confine`)
//! ```
//!
//! The guest sees `--root` as `/` and `--workspace` mounted at `/workspace`, where the command runs.
//! Guest stdout and stderr arrive on this process's stdout and stderr.

use std::ffi::{c_char, CString};
use std::process::exit;

#[link(name = "krun")]
extern "C" {
    fn krun_create_ctx() -> i32;
    fn krun_set_vm_config(ctx: u32, num_vcpus: u8, ram_mib: u32) -> i32;
    fn krun_set_root(ctx: u32, root_path: *const c_char) -> i32;
    fn krun_add_virtiofs(ctx: u32, tag: *const c_char, path: *const c_char) -> i32;
    fn krun_disable_implicit_vsock(ctx: u32) -> i32;
    fn krun_set_exec(
        ctx: u32,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> i32;
    fn krun_start_enter(ctx: u32) -> i32;
}

const USAGE: &str = "usage: playbox-krun-helper --root DIR --workspace DIR --cpus N --mem MIB [--network] [--secrets DIR] [--cgroup DIR] [--jail-dir DIR] [--rw DIR].. [--ro DIR].. [--seccomp] -- CMD [ARG...]";

/// Mount the workspace share, enter it, and hand over to the command.
const GUEST_SCRIPT: &str = r#"mkdir -p /workspace && mount -t virtiofs workspace /workspace || exit 125; cd /workspace || exit 125; exec "$@""#;

/// The same, with the host's secrets at /run/secrets (read-only, outside the workspace).
const GUEST_SCRIPT_SECRETS: &str = r#"mkdir -p /workspace /run/secrets && mount -t virtiofs workspace /workspace && mount -t virtiofs -o ro secrets /run/secrets || exit 125; cd /workspace || exit 125; exec "$@""#;

fn die(msg: &str) -> ! {
    eprintln!("playbox-krun-helper: {msg}");
    exit(125)
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| die("argument contains a NUL byte"))
}

fn check(what: &str, rc: i32) {
    if rc < 0 {
        die(&format!("{what} failed with {rc}"));
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut root, mut workspace, mut cpus, mut mem, mut network) =
        (None, None, 1u8, 512u32, false);
    let mut secrets: Option<String> = None;
    let (mut cgroup, mut jail_dir, mut seccomp) = (None::<String>, None::<String>, false);
    let (mut rw, mut ro): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut cmd: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => root = args.next(),
            "--workspace" => workspace = args.next(),
            "--cpus" => {
                cpus = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| die("bad --cpus"))
            }
            "--mem" => {
                mem = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| die("bad --mem"))
            }
            "--network" => network = true,
            "--secrets" => secrets = args.next(),
            "--cgroup" => cgroup = args.next(),
            "--jail-dir" => jail_dir = args.next(),
            "--rw" => rw.extend(args.next()),
            "--ro" => ro.extend(args.next()),
            "--seccomp" => seccomp = true,
            "--" => {
                cmd = args.by_ref().collect();
                break;
            }
            _ => die(USAGE),
        }
    }
    let (Some(root), Some(workspace)) = (root, workspace) else {
        die(USAGE)
    };
    if cmd.is_empty() || cpus == 0 {
        die(USAGE);
    }

    confine(cgroup, jail_dir, &rw, &ro, network, seccomp);

    let script = if secrets.is_some() { GUEST_SCRIPT_SECRETS } else { GUEST_SCRIPT };
    let (root, workspace, tag) = (c(&root), c(&workspace), c("workspace"));
    let secrets_dir = secrets.as_deref().map(c);
    let secrets_tag = c("secrets");
    // An explicit environment: with NULL libkrun copies ours onto the guest kernel command line,
    // which overflows for a normal shell environment.
    let env_strings = [
        c("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
        c("HOME=/root"),
        c("TERM=dumb"),
    ];
    let mut envp: Vec<*const c_char> = env_strings.iter().map(|s| s.as_ptr()).collect();
    envp.push(std::ptr::null());

    let mut argv_strings = vec![c("-c"), c(script), c("playbox")];
    argv_strings.extend(cmd.iter().map(|s| c(s)));
    let mut argv: Vec<*const c_char> = argv_strings.iter().map(|s| s.as_ptr()).collect();
    argv.push(std::ptr::null());

    // SAFETY: every pointer passed below comes from a `CString` (`root`, `workspace`, `tag`, `sh`,
    // `env_strings`, `argv_strings`) that stays alive until `krun_start_enter` takes over the process.
    // `argv` and `envp` are null-terminated arrays of those pointers. `ctx` is only converted to `u32`
    // after `check` has rejected a negative return value.
    unsafe {
        let ctx = krun_create_ctx();
        check("krun_create_ctx", ctx);
        let ctx = ctx as u32;
        check("krun_set_vm_config", krun_set_vm_config(ctx, cpus, mem));
        check("krun_set_root", krun_set_root(ctx, root.as_ptr()));
        check(
            "krun_add_virtiofs",
            krun_add_virtiofs(ctx, tag.as_ptr(), workspace.as_ptr()),
        );
        if let Some(dir) = &secrets_dir {
            check("krun_add_virtiofs (secrets)", krun_add_virtiofs(ctx, secrets_tag.as_ptr(), dir.as_ptr()));
        }
        if !network {
            // No vsock device means no TSI, and so no network of any kind.
            check(
                "krun_disable_implicit_vsock",
                krun_disable_implicit_vsock(ctx),
            );
        }
        let sh = c("/bin/sh");
        check(
            "krun_set_exec",
            krun_set_exec(ctx, sh.as_ptr(), argv.as_ptr(), envp.as_ptr()),
        );
        // Only returns on a configuration error; otherwise the process exits with the guest's code.
        let rc = krun_start_enter(ctx);
        die(&format!("krun_start_enter returned {rc}"));
    }
}

/// Join the run's cgroup, enter the jail and install the syscall filter, in that order, before any thread exists. Any failure exits:
/// a helper that was asked to confine itself never boots a VM unconfined.
#[cfg(target_os = "linux")]
fn confine(cgroup: Option<String>, jail_dir: Option<String>, rw: &[String], ro: &[String], network: bool, seccomp: bool) {
    use playbox_runner::{confine::{library_dirs, Plan}, jail::Jail};
    use std::path::PathBuf;
    let jail = jail_dir.map(|dir| {
        let mut read_only: Vec<PathBuf> = library_dirs();
        read_only.extend(ro.iter().map(PathBuf::from));
        if std::path::Path::new("/etc/ld.so.cache").exists() {
            read_only.push("/etc/ld.so.cache".into());
        }
        Jail {
            dir: dir.into(),
            read_only,
            read_write: rw.iter().map(PathBuf::from).collect(),
            devices: ["/dev/kvm", "/dev/null", "/dev/zero", "/dev/urandom"].iter().map(PathBuf::from).filter(|p| p.exists()).collect(),
            network,
        }
    });
    let plan = Plan { cgroup: cgroup.map(PathBuf::from), jail, seccomp };
    if plan.cgroup.is_some() || plan.jail.is_some() || plan.seccomp {
        if let Err(e) = plan.enter() {
            die(&format!("cannot confine myself, so not booting: {e:#}"));
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn confine(cgroup: Option<String>, jail_dir: Option<String>, _rw: &[String], _ro: &[String], _network: bool, seccomp: bool) {
    if cgroup.is_some() || jail_dir.is_some() || seccomp {
        die("confinement flags need Linux");
    }
}
