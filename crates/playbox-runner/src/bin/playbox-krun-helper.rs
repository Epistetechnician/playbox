//! Boots one libkrun microVM, runs a command in it, and exits with the command's exit code.
//!
//! libkrun's `krun_start_enter` never returns: it takes over the process and calls `exit()` when the
//! guest shuts down. So every task runs in its own short-lived helper process, started by
//! `KrunRunner`. On macOS this binary must be signed with the `com.apple.security.hypervisor`
//! entitlement (`scripts/install-krun.sh` does that).
//!
//! ```text
//! playbox-krun-helper --root DIR --workspace DIR --cpus N --mem MIB [--network] -- CMD [ARG...]
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

const USAGE: &str = "usage: playbox-krun-helper --root DIR --workspace DIR --cpus N --mem MIB [--network] -- CMD [ARG...]";

/// Mount the workspace share, enter it, and hand over to the command.
const GUEST_SCRIPT: &str = r#"mkdir -p /workspace && mount -t virtiofs workspace /workspace || exit 125; cd /workspace || exit 125; exec "$@""#;

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

    let (root, workspace, tag) = (c(&root), c(&workspace), c("workspace"));
    // An explicit environment: with NULL libkrun copies ours onto the guest kernel command line,
    // which overflows for a normal shell environment.
    let env_strings = [
        c("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
        c("HOME=/root"),
        c("TERM=dumb"),
    ];
    let mut envp: Vec<*const c_char> = env_strings.iter().map(|s| s.as_ptr()).collect();
    envp.push(std::ptr::null());

    let mut argv_strings = vec![c("-c"), c(GUEST_SCRIPT), c("playbox")];
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
