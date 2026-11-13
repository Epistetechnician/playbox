//! A syscall filter for the helper process, installed after the jail and before libkrun starts any thread.
//!
//! This is a **deny-list**, not Peerspan's allow-list (`crates/runtime/src/seccomp.rs`). An allow-list has to be measured against
//! the exact libkrun build and the guest workloads, and a call left off it kills the VM at the first use; that measurement needs a
//! KVM host and has not been done. A deny-list is safe to ship now: it removes the calls a compromised VMM would use to get out of
//! the jail or into the host kernel (tracing, mounting, namespaces, module and BPF loading, key rings, kexec) and leaves
//! everything else to the jail and the cgroup. The denied calls fail with `EPERM`, so a probing process sees an error, not a crash.
//!
//! The filter checks the architecture first and kills the process on a mismatch, so a 32-bit syscall number cannot alias a denied
//! 64-bit one. Threads created afterwards inherit it.

use anyhow::{bail, Context, Result};

/// Names of the denied syscalls, for the attestation text and the tests.
pub const DENIED: &[&str] = &[
    "ptrace", "process_vm_readv", "process_vm_writev", "mount", "umount2", "pivot_root", "chroot", "setns", "unshare", "kexec_load",
    "kexec_file_load", "init_module", "finit_module", "delete_module", "bpf", "perf_event_open", "keyctl", "add_key", "request_key",
    "reboot", "swapon", "swapoff", "open_by_handle_at", "mount_setattr", "move_mount", "fsopen", "fsmount", "fspick", "open_tree",
    "acct", "iopl", "ioperm", "settimeofday", "clock_settime", "adjtimex", "syslog", "quotactl",
];

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;

/// The syscall number of `name` on this architecture, `None` if it does not exist there (x86 port I/O, for instance).
fn number(name: &str) -> Option<u32> {
    let n = |v: libc::c_long| u32::try_from(v).ok();
    Some(match name {
        "ptrace" => n(libc::SYS_ptrace)?,
        "process_vm_readv" => n(libc::SYS_process_vm_readv)?,
        "process_vm_writev" => n(libc::SYS_process_vm_writev)?,
        "mount" => n(libc::SYS_mount)?,
        "umount2" => n(libc::SYS_umount2)?,
        "pivot_root" => n(libc::SYS_pivot_root)?,
        "chroot" => n(libc::SYS_chroot)?,
        "setns" => n(libc::SYS_setns)?,
        "unshare" => n(libc::SYS_unshare)?,
        "kexec_load" => n(libc::SYS_kexec_load)?,
        "init_module" => n(libc::SYS_init_module)?,
        "finit_module" => n(libc::SYS_finit_module)?,
        "delete_module" => n(libc::SYS_delete_module)?,
        "bpf" => n(libc::SYS_bpf)?,
        "perf_event_open" => n(libc::SYS_perf_event_open)?,
        "keyctl" => n(libc::SYS_keyctl)?,
        "add_key" => n(libc::SYS_add_key)?,
        "request_key" => n(libc::SYS_request_key)?,
        "reboot" => n(libc::SYS_reboot)?,
        "swapon" => n(libc::SYS_swapon)?,
        "swapoff" => n(libc::SYS_swapoff)?,
        "open_by_handle_at" => n(libc::SYS_open_by_handle_at)?,
        "acct" => n(libc::SYS_acct)?,
        "settimeofday" => n(libc::SYS_settimeofday)?,
        "clock_settime" => n(libc::SYS_clock_settime)?,
        "adjtimex" => n(libc::SYS_adjtimex)?,
        "syslog" => n(libc::SYS_syslog)?,
        "quotactl" => n(libc::SYS_quotactl)?,
        // The mount API and kexec_file_load have no constant in every libc release; the numbers are the same on both
        // architectures except the last.
        "open_tree" => 428,
        "move_mount" => 429,
        "fsopen" => 430,
        "fsmount" => 432,
        "fspick" => 433,
        "mount_setattr" => 442,
        "kexec_file_load" => {
            if cfg!(target_arch = "x86_64") {
                320
            } else {
                294
            }
        }
        #[cfg(target_arch = "x86_64")]
        "iopl" => n(libc::SYS_iopl)?,
        #[cfg(target_arch = "x86_64")]
        "ioperm" => n(libc::SYS_ioperm)?,
        _ => return None,
    })
}

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

fn insn(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// The filter program: architecture check, then one compare per denied number.
#[must_use]
pub fn program() -> Vec<libc::sock_filter> {
    let nums: Vec<u32> = DENIED.iter().filter_map(|n| number(n)).collect();
    // offsetof(struct seccomp_data, nr) = 0, offsetof(arch) = 4
    let mut p = vec![
        insn(BPF_LD_W_ABS, 0, 0, 4),
        insn(BPF_JEQ_K, 1, 0, AUDIT_ARCH),
        insn(BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS),
        insn(BPF_LD_W_ABS, 0, 0, 0),
    ];
    let count = nums.len();
    for (i, nr) in nums.iter().enumerate() {
        // On a match jump to the EPERM return, which sits after the remaining compares and the ALLOW.
        let to_deny = u8::try_from(count - i).unwrap_or(u8::MAX);
        p.push(insn(BPF_JEQ_K, to_deny, 0, *nr));
    }
    p.push(insn(BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW));
    p.push(insn(BPF_RET_K, 0, 0, SECCOMP_RET_ERRNO | libc::EPERM as u32));
    p
}

/// Install the filter on this thread and the threads it creates. Needs `no_new_privs`, which the jail sets.
///
/// # Errors
///
/// Fails if the kernel rejects the program; the caller must then exit rather than run unfiltered.
pub fn install() -> Result<()> {
    let prog = program();
    if prog.len() > 4000 {
        bail!("seccomp program too long");
    }
    let fprog = libc::sock_fprog { len: u16::try_from(prog.len()).context("program length")?, filter: prog.as_ptr().cast_mut() };
    // SAFETY: prctl with a valid sock_fprog that outlives the call; the kernel copies the program.
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        bail!("no_new_privs: {}", std::io::Error::last_os_error());
    }
    // SAFETY: as above.
    let rc = unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &fprog) };
    if rc != 0 {
        bail!("seccomp: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// Text for the host's signed statement: which calls the helper cannot make.
#[must_use]
pub fn describe() -> String {
    format!("seccomp=deny:{}", blake3::hash(DENIED.join(",").as_bytes()).to_hex().get(..16).unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_denied_call_has_a_number_on_this_machine_or_is_architecture_specific() {
        let missing: Vec<_> = DENIED.iter().filter(|n| number(n).is_none()).collect();
        assert!(missing.is_empty() || cfg!(not(target_arch = "x86_64")), "{missing:?}");
        assert!(missing.iter().all(|n| matches!(**n, "iopl" | "ioperm")), "{missing:?}");
    }

    #[test]
    fn the_program_is_well_formed() {
        let p = program();
        assert!(p.len() > DENIED.len() / 2 && p.len() < 100);
        // Every conditional jump lands inside the program.
        for (i, ins) in p.iter().enumerate() {
            if ins.code == BPF_JEQ_K {
                assert!(i + 1 + usize::from(ins.jt) < p.len() && i + 1 + usize::from(ins.jf) < p.len());
            }
        }
        assert_eq!(p.last().map(|i| i.k), Some(SECCOMP_RET_ERRNO | libc::EPERM as u32));
    }
}
