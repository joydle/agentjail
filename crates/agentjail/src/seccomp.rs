//! Seccomp-BPF syscall filtering.

use crate::config::SeccompLevel;
use crate::error::{JailError, Result};
use seccompiler::{SeccompAction, SeccompFilter, TargetArch};
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
pub fn compile(level: SeccompLevel) -> Result<Option<CompiledFilter>> {
    let filter = match level {
        SeccompLevel::Disabled => return Ok(None),
        SeccompLevel::Standard => build_standard_filter()?,
        SeccompLevel::Strict => build_strict_filter()?,
    };
    let bpf: seccompiler::BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| JailError::Seccomp(e.to_string()))?;
    Ok(Some(bpf))
}

/// Apply a pre-compiled filter. Zero-allocation on the child-side
/// fast path.
pub fn apply_compiled(bpf: &CompiledFilter) -> Result<()> {
    seccompiler::apply_filter(bpf).map_err(|e| JailError::Seccomp(e.to_string()))?;
    Ok(())
}


/// Syscalls blocked in both Standard and Strict modes.
fn base_blocked_syscalls() -> Vec<i64> {
    #[allow(unused_mut)]
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
        // clone3 — can pass namespace flags to create new namespaces,
        // bypassing the unshare/setns block.
        libc::SYS_clone3,
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

    let mut rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = blocked_syscalls
        .iter()
        .map(|&num| (num, vec![]))
        .collect();

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

    SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        ARCH,
    )
    .map_err(|e| JailError::Seccomp(e.to_string()))
}
