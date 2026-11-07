//! Seccomp-BPF syscall filtering.

use crate::config::SeccompLevel;
use crate::error::{JailError, Result};
use seccompiler::{SeccompAction, SeccompFilter, TargetArch, sock_filter};
use std::collections::BTreeMap;

#[cfg(target_arch = "x86_64")]
const ARCH: TargetArch = TargetArch::x86_64;

#[cfg(target_arch = "aarch64")]
const ARCH: TargetArch = TargetArch::aarch64;

/// Compiled BPF program ready to load into the kernel.
///
/// Reusable across spawns — the same program is valid for every child
/// under the same `SeccompLevel`. Built once in `Jail::new` and applied
/// via [`apply_compiled`] in the child.
pub type CompiledFilter = seccompiler::BpfProgram;

/// Compile a filter for the given level. `None` for `Disabled` —
/// the child skips the syscall entirely.
///
/// The program is a hand-written [`prelude`] (architecture check, x32
/// kill, `clone3` → `ENOSYS`) followed by the seccompiler-generated
/// blocklist. BPF jumps are relative, so the two concatenate cleanly.
pub fn compile(level: SeccompLevel) -> Result<Option<CompiledFilter>> {
    let filter = match level {
        SeccompLevel::Disabled => return Ok(None),
        SeccompLevel::Standard => build_standard_filter()?,
        SeccompLevel::Strict => build_strict_filter()?,
    };
    let body: seccompiler::BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| JailError::Seccomp(e.to_string()))?;
    let mut bpf = prelude(ARCH);
    bpf.extend(body);
    if bpf.len() > BPF_MAX_LEN {
        return Err(JailError::Seccomp(format!(
            "filter has {} instructions, kernel limit is {BPF_MAX_LEN}",
            bpf.len()
        )));
    }
    Ok(Some(bpf))
}

// --- Hand-written prelude ---------------------------------------------------

const BPF_MAX_LEN: usize = 4096;

const BPF_LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
const BPF_JMP_JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
const BPF_JMP_JGE_K: u16 = 0x35; // BPF_JMP | BPF_JGE | BPF_K
const BPF_RET_K: u16 = 0x06;

const SECCOMP_DATA_NR: u32 = 0;
const SECCOMP_DATA_ARCH: u32 = 4;

pub(crate) const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
pub(crate) const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
#[cfg(test)]
pub(crate) const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

pub(crate) const AUDIT_ARCH_X86_64: u32 = 62 | 0x8000_0000 | 0x4000_0000;
pub(crate) const AUDIT_ARCH_AARCH64: u32 = 183 | 0x8000_0000 | 0x4000_0000;

/// x32 syscalls run on the x86_64 audit arch with this bit set in the
/// syscall number. A filter that only checks `AUDIT_ARCH_X86_64` (as
/// seccompiler does) never matches them against the x86_64 numbers in
/// the blocklist, so every x32 variant of a blocked syscall would slip
/// through.
pub(crate) const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// `clone3` has the same number on every architecture we support.
const NR_CLONE3: u32 = 435;

fn stmt(code: u16, k: u32) -> sock_filter {
    sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> sock_filter {
    sock_filter { code, jt, jf, k }
}

/// Instructions that run before the blocklist:
///
/// 1. Kill the process on any foreign audit arch (i386 / compat
///    `int 0x80` on x86_64, aarch32 on aarch64): their syscall numbers
///    differ, so the blocklist would be meaningless.
/// 2. x86_64 only: kill any syscall number with the x32 bit set.
/// 3. `clone3` → `ENOSYS`. Its flags live in a struct seccomp can't
///    inspect, so it can't be argument-filtered like `clone`. `ENOSYS`
///    (not `EPERM`) makes glibc fall back to `clone`, which *is* filtered
///    — threads and `posix_spawn` keep working.
pub(crate) fn prelude(arch: TargetArch) -> Vec<sock_filter> {
    let audit_arch = match arch {
        TargetArch::x86_64 => AUDIT_ARCH_X86_64,
        TargetArch::aarch64 => AUDIT_ARCH_AARCH64,
        #[allow(unreachable_patterns)]
        _ => 0,
    };
    let mut v = vec![
        stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH),
        jump(BPF_JMP_JEQ_K, audit_arch, 1, 0),
        stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR),
    ];
    if matches!(arch, TargetArch::x86_64) {
        v.push(jump(BPF_JMP_JGE_K, X32_SYSCALL_BIT, 0, 1));
        v.push(stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS));
    }
    v.push(jump(BPF_JMP_JEQ_K, NR_CLONE3, 0, 1));
    v.push(stmt(BPF_RET_K, SECCOMP_RET_ERRNO | libc::ENOSYS as u32));
    v
}

/// `clone(2)` flags that create a namespace. `clone` with any of them is
/// rejected with `EPERM`, closing the same door `unshare`/`setns` close.
pub(crate) const CLONE_NS_FLAGS: &[u64] = &[
    libc::CLONE_NEWNS as u64,
    libc::CLONE_NEWUTS as u64,
    libc::CLONE_NEWIPC as u64,
    libc::CLONE_NEWUSER as u64,
    libc::CLONE_NEWPID as u64,
    libc::CLONE_NEWNET as u64,
    libc::CLONE_NEWCGROUP as u64,
];

/// Apply a pre-compiled filter. Zero-allocation on the child-side
/// fast path.
pub fn apply_compiled(bpf: &CompiledFilter) -> Result<()> {
    seccompiler::apply_filter(bpf).map_err(|e| JailError::Seccomp(e.to_string()))?;
    Ok(())
}

/// Syscalls blocked in both Standard and Strict modes.
fn base_blocked_syscalls() -> Vec<i64> {
    #[allow(unused_mut)] // only x86_64 pushes more below
    let mut v = vec![
        // Module loading
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        // Reboot / shutdown
        libc::SYS_reboot,
        libc::SYS_kexec_load,
        // Process tracing / introspection
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        // Mount (old API)
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        // Mount (new API, Linux 5.2+) — bypasses SYS_mount block
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        // Namespace escape
        libc::SYS_unshare,
        libc::SYS_setns,
        // Host identity
        libc::SYS_sethostname,
        libc::SYS_setdomainname,
        // Accounting / swap
        libc::SYS_acct,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        // Clock manipulation
        libc::SYS_settimeofday,
        libc::SYS_clock_settime,
        libc::SYS_adjtimex,
        // Keyring
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_keyctl,
        // BPF / perf — information leak and privilege escalation
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        // Exploitable race condition primitives
        libc::SYS_userfaultfd,
        // io_uring — bypasses ALL other seccomp-blocked syscalls because
        // the kernel performs operations on behalf of the process.
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        // Personality — PER_LINUX32 switches to 32-bit compat mode where
        // syscall numbers differ, defeating the entire filter.
        libc::SYS_personality,
        // clone3 is handled by the prelude (ENOSYS, see `prelude`);
        // clone is argument-filtered in `build_blocklist_filter`.
        // memfd_create — creates anonymous executable memory regions,
        // bypassing NOEXEC mount flags.
        libc::SYS_memfd_create,
        // mount_setattr (Linux 5.12+) — can strip RDONLY, NOEXEC, NOSUID
        // from existing mounts, defeating bind-mount protections.
        libc::SYS_mount_setattr,
        // chroot — the jail uses pivot_root + detach, so chroot is never
        // needed inside. Left unblocked it enables the classic nested-
        // chroot + fchdir("..") escape.
        libc::SYS_chroot,
        // File-handle reopen (CAP_DAC_READ_SEARCH-gated) — name_to_handle_at
        // encodes a path as a 32-byte handle, open_by_handle_at resolves
        // it against any mount of the same fs. Escapes a bind mount if
        // the underlying fs is shared with a more permissive mount.
        libc::SYS_name_to_handle_at,
        libc::SYS_open_by_handle_at,
        // Device-node creation — defence-in-depth; writable mounts are
        // already NODEV so a bogus mknod can't be opened, but blocking
        // the syscall means the node never lands on disk in the first
        // place (matters for snapshots).
        libc::SYS_mknodat,
        // Host-wide fanotify — marks can be placed on mounts the
        // attacker doesn't own (CAP_SYS_ADMIN-gated in init userns).
        libc::SYS_fanotify_init,
        // Quota control — turn on/off fs quotas, DoS on shared disks.
        libc::SYS_quotactl,
        // Kernel syslog ring buffer — read or clear dmesg. Information
        // leak (kptr, kernel-stack traces) and evidence destruction.
        libc::SYS_syslog,
    ];

    // iopl/ioperm are x86-only (hardware port I/O).
    // SYS_mknod is also x86_64-only — aarch64 exposes only mknodat.
    #[cfg(target_arch = "x86_64")]
    {
        v.push(libc::SYS_iopl);
        v.push(libc::SYS_ioperm);
        v.push(libc::SYS_kexec_file_load);
        v.push(libc::SYS_mknod);
    }

    v
}

fn build_standard_filter() -> Result<SeccompFilter> {
    build_blocklist_filter(&base_blocked_syscalls())
}

fn build_strict_filter() -> Result<SeccompFilter> {
    // Everything in Standard, plus block network socket creation.
    let mut blocked = base_blocked_syscalls();
    blocked.extend_from_slice(&[
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_sendto,
        libc::SYS_recvfrom,
        libc::SYS_sendmsg,
        libc::SYS_recvmsg,
        libc::SYS_sendmmsg,
        libc::SYS_recvmmsg,
        libc::SYS_shutdown,
    ]);
    build_blocklist_filter(&blocked)
}

fn build_blocklist_filter(blocked_syscalls: &[i64]) -> Result<SeccompFilter> {
    // Default action: Allow (most syscalls pass through)
    // Match action: Errno (blocked syscalls return EPERM)
    // Empty rules vec = match unconditionally for that syscall number

    let mut rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> =
        blocked_syscalls.iter().map(|&num| (num, vec![])).collect();

    // Conditional block: `ioctl` with request == TIOCSTI.
    //
    // TIOCSTI pushes characters back into the input queue of the
    // terminal on the given fd. If the jailed process still holds
    // an fd referring to the parent's controlling tty (possible
    // when stdin wasn't redirected), this lets it inject keystrokes
    // as if the operator typed them. `setsid()` mitigates the
    // common case but argument-filtering closes the door for good.
    //
    // Only `ioctl` with arg[1] == 0x5412 is rejected; any other
    // `ioctl` (e.g. `FIONBIO`, terminal-size queries) still passes.
    const TIOCSTI: u64 = 0x5412;
    let tiocsti_cond = seccompiler::SeccompCondition::new(
        1,
        seccompiler::SeccompCmpArgLen::Dword,
        seccompiler::SeccompCmpOp::Eq,
        TIOCSTI,
    )
    .map_err(|e| JailError::Seccomp(e.to_string()))?;
    let tiocsti_rule = seccompiler::SeccompRule::new(vec![tiocsti_cond])
        .map_err(|e| JailError::Seccomp(e.to_string()))?;
    rules.insert(libc::SYS_ioctl, vec![tiocsti_rule]);

    // Conditional block: `socket(domain, ...)` with dangerous
    // families.
    //   AF_NETLINK (16) — observability/control channels; some
    //                     families are per-netns but others
    //                     (NETLINK_AUDIT, NETLINK_KOBJECT_UEVENT
    //                     subsystem-dependent) reach host state.
    //   AF_PACKET  (17) — raw packet capture / injection.
    //   AF_VSOCK   (40) — virtio socket to the hypervisor.
    //
    // The fd itself is a probing oracle even with caps dropped;
    // blocking the `socket()` call closes that door. AF_UNIX (1),
    // AF_INET (2), AF_INET6 (10) stay available. Strict mode already
    // blocks `socket` unconditionally via the base list, so only
    // install the arg-filter when the key is still absent.
    if let std::collections::btree_map::Entry::Vacant(entry) = rules.entry(libc::SYS_socket) {
        let mut socket_rules = Vec::new();
        for af in [libc::AF_NETLINK, libc::AF_PACKET, libc::AF_VSOCK] {
            let cond = seccompiler::SeccompCondition::new(
                0,
                seccompiler::SeccompCmpArgLen::Dword,
                seccompiler::SeccompCmpOp::Eq,
                af as u64,
            )
            .map_err(|e| JailError::Seccomp(e.to_string()))?;
            socket_rules.push(
                seccompiler::SeccompRule::new(vec![cond])
                    .map_err(|e| JailError::Seccomp(e.to_string()))?,
            );
        }
        entry.insert(socket_rules);
    }

    // Conditional block: `clone(flags, ...)` with any namespace flag.
    // One rule per flag (rules are OR'ed): (flags & F) == F. Namespace
    // flags all sit in the low 32 bits, so a Dword compare is exact.
    let mut clone_rules = Vec::new();
    for &flag in CLONE_NS_FLAGS {
        let cond = seccompiler::SeccompCondition::new(
            0,
            seccompiler::SeccompCmpArgLen::Dword,
            seccompiler::SeccompCmpOp::MaskedEq(flag),
            flag,
        )
        .map_err(|e| JailError::Seccomp(e.to_string()))?;
        clone_rules.push(
            seccompiler::SeccompRule::new(vec![cond])
                .map_err(|e| JailError::Seccomp(e.to_string()))?,
        );
    }
    rules.insert(libc::SYS_clone, clone_rules);

    SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        ARCH,
    )
    .map_err(|e| JailError::Seccomp(e.to_string()))
}

#[cfg(test)]
mod tests {
    //! Interpret the generated BPF directly. This covers paths the
    //! aarch64 test host can't execute (x32, i386 compat) and pins the
    //! clone/clone3 behaviour independently of the integration tests.
    use super::*;

    const AUDIT_ARCH_I386: u32 = 3 | 0x4000_0000;

    /// Minimal classic-BPF interpreter for seccomp programs: the opcode
    /// subset seccompiler and `prelude` emit.
    fn run(prog: &[sock_filter], nr: u32, arch: u32, args: [u64; 6]) -> u32 {
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(&nr.to_le_bytes());
        data[4..8].copy_from_slice(&arch.to_le_bytes());
        for (i, a) in args.iter().enumerate() {
            data[16 + 8 * i..24 + 8 * i].copy_from_slice(&a.to_le_bytes());
        }
        let mut acc: u32 = 0;
        let mut pc = 0usize;
        loop {
            let ins = &prog[pc];
            pc += 1;
            match ins.code {
                0x20 => {
                    let o = ins.k as usize;
                    acc = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
                }
                0x54 => acc &= ins.k,
                0x05 => pc += ins.k as usize,
                0x15 | 0x25 | 0x35 | 0x45 => {
                    let t = match ins.code {
                        0x15 => acc == ins.k,
                        0x25 => acc > ins.k,
                        0x35 => acc >= ins.k,
                        _ => acc & ins.k != 0,
                    };
                    pc += if t { ins.jt } else { ins.jf } as usize;
                }
                0x06 => return ins.k,
                op => panic!("unsupported BPF opcode {op:#x} at {}", pc - 1),
            }
        }
    }

    fn host_arch() -> u32 {
        if cfg!(target_arch = "x86_64") {
            AUDIT_ARCH_X86_64
        } else {
            AUDIT_ARCH_AARCH64
        }
    }

    fn eperm() -> u32 {
        SECCOMP_RET_ERRNO | libc::EPERM as u32
    }

    #[test]
    fn clone_namespace_flags_are_rejected() {
        for level in [SeccompLevel::Standard, SeccompLevel::Strict] {
            let prog = compile(level).unwrap().unwrap();
            let clone = libc::SYS_clone as u32;
            for &flag in CLONE_NS_FLAGS {
                let flags = flag | libc::SIGCHLD as u64;
                assert_eq!(
                    run(&prog, clone, host_arch(), [flags, 0, 0, 0, 0, 0]),
                    eperm()
                );
            }
            // All of them at once (0x7E020000).
            assert_eq!(
                run(&prog, clone, host_arch(), [0x7E02_0000, 0, 0, 0, 0, 0]),
                eperm()
            );
            // glibc fork(): SIGCHLD | CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID.
            let fork_flags = 0x0120_0011;
            assert_eq!(
                run(&prog, clone, host_arch(), [fork_flags, 0, 0, 0, 0, 0]),
                SECCOMP_RET_ALLOW
            );
            // glibc pthread_create() clone fallback flags.
            let thread_flags = 0x003d_0f00;
            assert_eq!(
                run(&prog, clone, host_arch(), [thread_flags, 0, 0, 0, 0, 0]),
                SECCOMP_RET_ALLOW
            );
        }
    }

    #[test]
    fn clone3_returns_enosys() {
        let prog = compile(SeccompLevel::Standard).unwrap().unwrap();
        assert_eq!(
            run(&prog, 435, host_arch(), [0; 6]),
            SECCOMP_RET_ERRNO | libc::ENOSYS as u32
        );
    }

    #[test]
    fn blocklist_and_allow_still_work() {
        let prog = compile(SeccompLevel::Standard).unwrap().unwrap();
        assert_eq!(
            run(&prog, libc::SYS_unshare as u32, host_arch(), [0; 6]),
            eperm()
        );
        assert_eq!(
            run(&prog, libc::SYS_getpid as u32, host_arch(), [0; 6]),
            SECCOMP_RET_ALLOW
        );
    }

    #[test]
    fn foreign_arch_is_killed() {
        let prog = compile(SeccompLevel::Standard).unwrap().unwrap();
        for arch in [AUDIT_ARCH_I386, 0x4000_0028 /* ARM (aarch32) */, 0] {
            assert_eq!(
                run(&prog, libc::SYS_getpid as u32, arch, [0; 6]),
                SECCOMP_RET_KILL_PROCESS
            );
        }
    }

    /// x86_64 prelude, checked by interpretation so it runs on any host
    /// (the docker test host is aarch64 and can't execute x32 code).
    #[test]
    fn x86_64_prelude_kills_x32_and_i386() {
        let mut prog = prelude(TargetArch::x86_64);
        prog.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
        // x32 unshare (272) and x32 read (0).
        for nr in [X32_SYSCALL_BIT | 272, X32_SYSCALL_BIT, u32::MAX] {
            assert_eq!(
                run(&prog, nr, AUDIT_ARCH_X86_64, [0; 6]),
                SECCOMP_RET_KILL_PROCESS
            );
        }
        // i386 compat syscalls (int 0x80) carry AUDIT_ARCH_I386.
        assert_eq!(
            run(&prog, 310, AUDIT_ARCH_I386, [0; 6]),
            SECCOMP_RET_KILL_PROCESS
        );
        // Native x86_64 syscalls fall through to the blocklist.
        assert_eq!(run(&prog, 39, AUDIT_ARCH_X86_64, [0; 6]), SECCOMP_RET_ALLOW);
        assert_eq!(
            run(&prog, 435, AUDIT_ARCH_X86_64, [0; 6]),
            SECCOMP_RET_ERRNO | libc::ENOSYS as u32
        );
    }

    #[test]
    fn aarch64_prelude_has_no_x32_rule() {
        let mut prog = prelude(TargetArch::aarch64);
        prog.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
        assert_eq!(
            run(&prog, 172, AUDIT_ARCH_AARCH64, [0; 6]),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            run(&prog, 172, AUDIT_ARCH_X86_64, [0; 6]),
            SECCOMP_RET_KILL_PROCESS
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_64_full_filter_kills_x32() {
        let prog = compile(SeccompLevel::Standard).unwrap().unwrap();
        let x32_unshare = X32_SYSCALL_BIT | libc::SYS_unshare as u32;
        assert_eq!(
            run(&prog, x32_unshare, AUDIT_ARCH_X86_64, [0; 6]),
            SECCOMP_RET_KILL_PROCESS
        );
    }
}
