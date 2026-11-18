#![allow(unsafe_code)] // Linux syscall boundary; each block carries its own justification
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
    "ptrace",
    "process_vm_readv",
    "process_vm_writev",
    "mount",
    "umount2",
    "pivot_root",
    "chroot",
    "setns",
    "unshare",
    "kexec_load",
    "kexec_file_load",
    "init_module",
    "finit_module",
    "delete_module",
    "bpf",
    "perf_event_open",
    "keyctl",
    "add_key",
    "request_key",
    "reboot",
    "swapon",
    "swapoff",
    "open_by_handle_at",
    "io_uring_setup",
    "io_uring_enter",
    "io_uring_register",
    "userfaultfd",
    "mount_setattr",
    "move_mount",
    "fsopen",
    "fsmount",
    "fspick",
    "open_tree",
    "acct",
    "iopl",
    "ioperm",
    "settimeofday",
    "clock_settime",
    "adjtimex",
    "syslog",
    "quotactl",
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
        "userfaultfd" => n(libc::SYS_userfaultfd)?,
        // Unified Linux syscall numbers (identical on x86_64 and aarch64) that are not in every libc release.
        "io_uring_setup" => 425,
        "io_uring_enter" => 426,
        "io_uring_register" => 427,
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
const BPF_JGE_K: u16 = 0x35;
const BPF_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

const SYS_CLONE3: u32 = 435;
/// `CLONE_NEWNS | CLONE_NEWCGROUP | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET`.
const CLONE_NS_MASK: u32 = 0x7e02_0000;
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

fn insn(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// The filter program: architecture check, x32 rejection (`x86_64`), `clone3` -> `ENOSYS` fallback,
/// denied syscalls -> `EPERM`, and `clone` namespace-flag inspection -> `EPERM`.
#[must_use]
pub fn program() -> Vec<libc::sock_filter> {
    let nums: Vec<u32> = DENIED.iter().filter_map(|n| number(n)).collect();
    let clone_nr = u32::try_from(libc::SYS_clone).expect("SYS_clone fits in u32");
    let x32_len: usize = usize::from(cfg!(target_arch = "x86_64"));
    // Header (4) + optional x32 check (0/1) + clone3 check (1) + DENIED (nums.len()) + clone check (3) + returns (3: ALLOW, ENOSYS, EPERM).
    let total = 4 + x32_len + 1 + nums.len() + 3 + 3;
    let idx_enosys = total - 2;
    let idx_eperm = total - 1;
    let jump_to = |cur: usize, target: usize| -> u8 {
        u8::try_from(target - (cur + 1)).expect("seccomp jump fits in u8")
    };

    // offsetof(struct seccomp_data, nr) = 0, offsetof(arch) = 4, offsetof(args[0]) = 16 (little-endian low 32 bits of flags)
    let mut p = Vec::with_capacity(total);
    p.push(insn(BPF_LD_W_ABS, 0, 0, 4));
    p.push(insn(BPF_JEQ_K, 1, 0, AUDIT_ARCH));
    p.push(insn(BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS));
    p.push(insn(BPF_LD_W_ABS, 0, 0, 0));

    #[cfg(target_arch = "x86_64")]
    {
        let cur = p.len();
        p.push(insn(BPF_JGE_K, jump_to(cur, idx_eperm), 0, X32_SYSCALL_BIT));
    }

    // Return ENOSYS for clone3 so libc cleanly falls back to clone(2), whose flags we can inspect.
    let cur = p.len();
    p.push(insn(BPF_JEQ_K, jump_to(cur, idx_enosys), 0, SYS_CLONE3));

    for nr in &nums {
        let cur = p.len();
        p.push(insn(BPF_JEQ_K, jump_to(cur, idx_eperm), 0, *nr));
    }

    // On both x86_64 and aarch64 Linux, clone(unsigned long flags, ...) passes `flags` in args[0].
    p.push(insn(BPF_JEQ_K, 0, 2, clone_nr));
    p.push(insn(BPF_LD_W_ABS, 0, 0, 16));
    let cur = p.len();
    p.push(insn(BPF_JSET_K, jump_to(cur, idx_eperm), 0, CLONE_NS_MASK));

    p.push(insn(BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW));
    p.push(insn(
        BPF_RET_K,
        0,
        0,
        SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
    ));
    p.push(insn(
        BPF_RET_K,
        0,
        0,
        SECCOMP_RET_ERRNO | libc::EPERM as u32,
    ));
    debug_assert_eq!(p.len(), total);
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
    let fprog = libc::sock_fprog {
        len: u16::try_from(prog.len()).context("program length")?,
        filter: prog.as_ptr().cast_mut(),
    };
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
    format!(
        "seccomp=deny:{}",
        blake3::hash(DENIED.join(",").as_bytes())
            .to_hex()
            .get(..16)
            .unwrap_or("")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_denied_call_has_a_number_on_this_machine_or_is_architecture_specific() {
        let missing: Vec<_> = DENIED.iter().filter(|n| number(n).is_none()).collect();
        assert!(
            missing.is_empty() || cfg!(not(target_arch = "x86_64")),
            "{missing:?}"
        );
        assert!(
            missing.iter().all(|n| matches!(**n, "iopl" | "ioperm")),
            "{missing:?}"
        );
    }

    fn eval(prog: &[libc::sock_filter], arch: u32, nr: u32, arg0: u64) -> u32 {
        let mut pc = 0usize;
        let mut acc = 0u32;
        loop {
            let ins = prog[pc];
            match ins.code {
                BPF_LD_W_ABS => {
                    acc = match ins.k {
                        0 => nr,
                        4 => arch,
                        16 => (arg0 & 0xffff_ffff) as u32,
                        20 => (arg0 >> 32) as u32,
                        other => panic!("unexpected seccomp_data offset {other}"),
                    };
                    pc += 1;
                }
                BPF_JEQ_K => {
                    pc += 1 + usize::from(if acc == ins.k { ins.jt } else { ins.jf });
                }
                BPF_JGE_K => {
                    pc += 1 + usize::from(if acc >= ins.k { ins.jt } else { ins.jf });
                }
                BPF_JSET_K => {
                    pc += 1 + usize::from(if (acc & ins.k) != 0 { ins.jt } else { ins.jf });
                }
                BPF_RET_K => return ins.k,
                other => panic!("unexpected BPF opcode {other:#x}"),
            }
        }
    }

    #[test]
    fn the_program_is_well_formed() {
        let p = program();
        assert!(p.len() > DENIED.len() / 2 && p.len() < 120);
        // Every conditional jump lands inside the program.
        for (i, ins) in p.iter().enumerate() {
            if matches!(ins.code, BPF_JEQ_K | BPF_JGE_K | BPF_JSET_K) {
                assert!(
                    i + 1 + usize::from(ins.jt) < p.len() && i + 1 + usize::from(ins.jf) < p.len()
                );
            }
        }
        let eperm = SECCOMP_RET_ERRNO | libc::EPERM as u32;
        let enosys = SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
        assert_eq!(p.last().map(|i| i.k), Some(eperm));

        assert_eq!(eval(&p, AUDIT_ARCH ^ 1, 0, 0), SECCOMP_RET_KILL_PROCESS);
        #[cfg(target_arch = "x86_64")]
        assert_eq!(eval(&p, AUDIT_ARCH, X32_SYSCALL_BIT | 1, 0), eperm);

        for name in [
            "io_uring_setup",
            "io_uring_enter",
            "io_uring_register",
            "userfaultfd",
            "open_by_handle_at",
            "unshare",
            "mount",
        ] {
            assert_eq!(
                eval(&p, AUDIT_ARCH, number(name).unwrap(), 0),
                eperm,
                "{name}"
            );
        }
        assert_eq!(eval(&p, AUDIT_ARCH, SYS_CLONE3, 0), enosys);

        let clone_nr = u32::try_from(libc::SYS_clone).unwrap();
        for ns_flag in [
            0x0002_0000u64, // CLONE_NEWNS
            0x0200_0000,    // CLONE_NEWCGROUP
            0x0400_0000,    // CLONE_NEWUTS
            0x0800_0000,    // CLONE_NEWIPC
            0x1000_0000,    // CLONE_NEWUSER
            0x2000_0000,    // CLONE_NEWPID
            0x4000_0000,    // CLONE_NEWNET
        ] {
            assert_eq!(eval(&p, AUDIT_ARCH, clone_nr, ns_flag), eperm);
        }
        // Normal thread / fork clone flags are allowed.
        assert_eq!(
            eval(&p, AUDIT_ARCH, clone_nr, 0x0001_0f00),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            eval(&p, AUDIT_ARCH, clone_nr, libc::SIGCHLD as u64),
            SECCOMP_RET_ALLOW
        );
        let read_nr = u32::try_from(libc::SYS_read).unwrap();
        assert_eq!(eval(&p, AUDIT_ARCH, read_nr, 0), SECCOMP_RET_ALLOW);
    }
}
