//! Child-side jail setup: runs post-fork, pre-exec.

use crate::config::{JailConfig, Network};
use crate::error::{JailError, Result};
use crate::namespace::{
    NamespaceConfig, enter_namespaces, enter_user_namespace_alone, setup_loopback,
};
use crate::veth::{proxy_env_vars, veth_addrs};
use crate::seccomp::{CompiledFilter, apply_compiled};
use crate::{gpu, landlock, mount, netlink};

use std::ffi::CString;

/// Setup the child process inside the jail.
///
/// `sync_fd`: For Allowlist mode, the child's end of a socketpair for syncing
/// with the parent. Child signals "in netns", parent replies with veth ID.
///
/// `userns_sync_fd`: When `config.user_namespace` is true, a socketpair
/// end the child uses to coordinate the user-namespace bring-up with
/// the parent: child enters NEWUSER, writes one byte, waits for one
/// byte back (parent wrote uid_map/gid_map), then proceeds.
///
/// `seccomp_filter`: Pre-compiled BPF program to install before `exec()`.
/// `None` when `config.seccomp == SeccompLevel::Disabled`. Built once in
/// `Jail::new` and shared across all spawns of the same `Jail`.
pub(crate) fn setup_child(
    config: &JailConfig,
    gpu_resources: Option<&gpu::NvidiaResources>,
    seccomp_filter: Option<&CompiledFilter>,
    cmd: &str,
    args: &[String],
    sync_fd: Option<i32>,
    userns_sync_fd: Option<i32>,
) -> Result<()> {
    // 0. User namespace FIRST, with parent-side uid_map write
    //    sandwiched. Rest of the unshares happen after so the child
    //    runs them under the new userns' caps, and — more importantly
    //    — so `/proc/<pid>/uid_map` targets the intended namespace.
    if config.user_namespace {
        let fd = userns_sync_fd.ok_or_else(|| {
            JailError::Exec(std::io::Error::other(
                "user_namespace=true requires a userns sync fd",
            ))
        })?;
        enter_user_namespace_alone()?;
        // Signal "userns created".
        // SAFETY: valid fd from socketpair, one-byte write.
        if unsafe { libc::write(fd, [1u8].as_ptr() as *const _, 1) } != 1 {
            unsafe { libc::_exit(127) };
        }
        // Wait for "maps written".
        let mut ack = [0u8; 1];
        // SAFETY: valid fd from socketpair, one-byte read.
        if unsafe { libc::read(fd, ack.as_mut_ptr() as *mut _, 1) } != 1 {
            unsafe { libc::_exit(127) };
        }
    }

    // 1. Enter remaining namespaces (user handled above, pid via
    //    double-fork below).
    let ns_config = NamespaceConfig {
        user: false,
        mount: true,
        pid: false,
        network: true,
        ipc: config.ipc_namespace,
    };
    enter_namespaces(ns_config)?;

    // 2. Setup network
    let mut proxy_host_ip = None;
    match &config.network {
        Network::None => {}
        Network::Loopback => {
            setup_loopback()?;
        }
        Network::Allowlist(_) => {
            if let Some(fd) = sync_fd {
                // Signal parent: "I've entered my network namespace"
                // SAFETY: Valid fd from socketpair, writing 1 byte.
                if unsafe { libc::write(fd, [1u8].as_ptr() as *const _, 1) } != 1 {
                    unsafe { libc::_exit(127) };
                }
                // Read veth ID from parent (4 bytes, LE u32)
                let mut id_buf = [0u8; 4];
                // SAFETY: Valid fd from socketpair, reading 4 bytes.
                if unsafe { libc::read(fd, id_buf.as_mut_ptr() as *mut _, 4) } != 4 {
                    unsafe { libc::_exit(127) };
                }
                // SAFETY: Done with sync channel.
                unsafe { libc::close(fd) };

                let veth_id = u32::from_le_bytes(id_buf);
                let (host_ip, jail_ip) = veth_addrs(veth_id);
                let jail_if = format!("aj-j{veth_id}");

                setup_loopback()?;
                netlink::add_ipv4_addr(&jail_if, jail_ip, 30)?;
                netlink::set_link_up(&jail_if)?;
                netlink::add_default_route(host_ip)?;

                proxy_host_ip = Some(host_ip);
            }
        }
    }

    // 3. Filesystem
    mount::make_root_private()?;
    // Unpredictable temp dir so a host-side attacker can't race a
    // symlink into place at a guessed path before setup_root runs.
    let new_root = mount::make_jail_root()?;
    mount::setup_root(
        &new_root,
        &config.source,
        &config.output,
        config.source_rw,
        &config.readonly_overlays,
    )?;

    if let Some(res) = gpu_resources {
        gpu::setup_mounts(&new_root, res)?;
    }

    // 4. Landlock
    if config.landlock {
        if !landlock::is_available() {
            // The config asked for landlock but the kernel can't
            // provide it (<5.13 or disabled). Soft-falling through
            // would hand back a jail without the FS defence the
            // caller explicitly requested, so refuse instead.
            return Err(JailError::Exec(std::io::Error::other(
                "config.landlock=true but kernel lacks LSM support \
                 (need Linux ≥5.13 with CONFIG_SECURITY_LANDLOCK). \
                 Set landlock=false to acknowledge the relaxed posture.",
            )));
        }
        let rules = [
            (config.source.as_path(), crate::config::Access::ReadOnly),
            (config.output.as_path(), crate::config::Access::ReadWrite),
        ];
        // apply_rules returns Err on any failure inside the ruleset
        // build or restrict_self — propagate rather than print and
        // continue. If we're past this point the child MUST have
        // landlock in force.
        landlock::apply_rules(&rules)?;
    }

    // 5. Pivot into the new root. pivot_root + MNT_DETACH fully
    //    removes the host filesystem from the mount namespace — a
    //    plain chroot(2) leaves the old root attached and is
    //    escapable via nested-chroot + fchdir + "..".
    mount::pivot_into(&new_root)?;
    std::env::set_current_dir("/workspace").map_err(JailError::Exec)?;

    // 6. Environment
    let mut env = config.env.clone();
    if let Some(host_ip) = proxy_host_ip {
        env.extend(proxy_env_vars(host_ip));
    }
    if gpu_resources.is_some() {
        env.extend(gpu::env_vars(&config.gpu));
    }
    if !config.readonly_overlays.is_empty() {
        prepend_flavor_bins_to_path(&mut env, &config.readonly_overlays);
    }

    // 7. Resource limits + privilege hardening.
    set_fd_limit(4096);
    // Prevent core dumps (could write sensitive memory to output dir).
    set_core_limit(0);
    // Prevent privilege escalation via setuid binaries, even if seccomp is disabled.
    // SAFETY: PR_SET_NO_NEW_PRIVS is always safe to set.
    unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };

    // Mark every inherited fd CLOEXEC. Doesn't need any capability
    // and doesn't affect setup — execve will close the flagged fds.
    close_inherited_fds();

    // 8. PID namespace double-fork. Capability drop happens in the
    //    grandchild AFTER the ns ops and proc remount (both need
    //    CAP_SYS_ADMIN); if we dropped here, unshare(NEWPID) and
    //    the grandchild's mount would both EPERM.
    if config.pid_namespace {
        enter_pid_namespace_and_exec(seccomp_filter, cmd, args, &env)?;
        unreachable!()
    }

    // No-pidns path: nothing else needs caps from here on, so drop
    // them now and then seccomp+exec.
    drop_all_capabilities();

    // 9. Seccomp (must be last before exec). The filter was compiled
    // in `Jail::new`; apply the cached program with no rebuild.
    if let Some(bpf) = seccomp_filter {
        apply_compiled(bpf)?;
    }

    // 10. Exec
    do_exec(cmd, args, &env)
}

/// Enter PID namespace via double-fork pattern.
///
/// After unshare(NEWPID), the current process is NOT in the new PID namespace.
/// Only children will be. So we fork, and the child becomes PID 1.
fn enter_pid_namespace_and_exec(
    seccomp_filter: Option<&CompiledFilter>,
    cmd: &str,
    args: &[String],
    env: &[(String, String)],
) -> Result<()> {
    use rustix::thread::{UnshareFlags, unshare};

    unshare(UnshareFlags::NEWPID).map_err(JailError::Namespace)?;

    // SAFETY: fork() is safe when we immediately either _exit() or exec() in child.
    let pid = unsafe { libc::fork() };

    match pid {
        -1 => Err(JailError::Fork(rustix::io::Errno::from_raw_os_error(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ))),
        0 => {
            // Re-arm parent-death signal (cleared by fork). If the
            // intermediate process dies we should die too, keeping the
            // chain: parent → child → grandchild all linked.
            unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };

            // Remount /proc for the new PID namespace. This MUST succeed
            // or the child sees the host process tree (critical info leak).
            if let Err(e) = remount_proc() {
                eprintln!("/proc remount failed (host PID leak): {e}");
                unsafe { libc::_exit(127) };
            }

            // Cap drop only after remount_proc (which needs
            // CAP_SYS_ADMIN in the mount ns). From here on we don't
            // need any capability — seccomp-load just needs NNP, and
            // execve needs none. An attacker-triggered syscall that
            // happens to slip seccomp still can't do anything
            // cap-gated because we hold none.
            drop_all_capabilities();

            if let Some(bpf) = seccomp_filter
                && let Err(e) = apply_compiled(bpf)
            {
                eprintln!("seccomp failed: {e}");
                unsafe { libc::_exit(127) };
            }

            if let Err(e) = do_exec(cmd, args, env) {
                eprintln!("exec failed: {e}");
                unsafe { libc::_exit(127) };
            }
            unreachable!()
        }
        child_pid => {
            let mut status: libc::c_int = 0;
            // SAFETY: Waiting for our own child with valid pointer.
            unsafe { libc::waitpid(child_pid, &mut status, 0) };

            let exit_code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                1
            };

            // SAFETY: Exiting the intermediate process cleanly.
            unsafe { libc::_exit(exit_code) };
        }
    }
}

/// Remount /proc for the new PID namespace.
///
/// Must preserve the same mount flags and `hidepid=invisible` option
/// that the outer child's initial `mount_proc` used. `umount2 +
/// mount()` lands a fresh superblock, which otherwise inherits the
/// kernel defaults (no hidepid, no NOSUID/NODEV/NOEXEC) — silently
/// undoing the hardening for any pidns-enabled jail.
fn remount_proc() -> Result<()> {
    // SAFETY: All hardcoded C string literals.
    let proc = c"/proc";
    let procfs = c"proc";
    let opts = c"hidepid=invisible";

    // SAFETY: umount2 with a valid C string; the MNT_DETACH flag
    // makes it lazy so no open /proc fds block the swap.
    unsafe { libc::umount2(proc.as_ptr(), libc::MNT_DETACH) };

    let flags = (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as libc::c_ulong;
    // SAFETY: all pointers are valid static C strings; flags/data
    // conform to mount(2) for procfs.
    let ret = unsafe {
        libc::mount(
            procfs.as_ptr(),
            proc.as_ptr(),
            procfs.as_ptr(),
            flags,
            opts.as_ptr() as *const libc::c_void,
        )
    };

    if ret == 0 { Ok(()) } else { Err(JailError::Exec(std::io::Error::last_os_error())) }
}

/// Execute the target command (never returns on success).
fn do_exec(cmd: &str, args: &[String], env: &[(String, String)]) -> Result<()> {
    let c_cmd =
        CString::new(cmd).map_err(|_| JailError::Exec(std::io::Error::other("invalid command")))?;

    let c_args: Vec<CString> = std::iter::once(Ok(c_cmd.clone()))
        .chain(args.iter().map(|a| {
            CString::new(a.as_str())
                .map_err(|_| JailError::Exec(std::io::Error::other(format!("argument contains null byte: {a:?}"))))
        }))
        .collect::<Result<Vec<_>>>()?;

    let c_args_ptrs: Vec<*const libc::c_char> = c_args
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let c_env: Vec<CString> = env
        .iter()
        .map(|(k, v)| {
            CString::new(format!("{k}={v}"))
                .map_err(|_| JailError::Exec(std::io::Error::other(format!("env var contains null byte: {k}={v}"))))
        })
        .collect::<Result<Vec<_>>>()?;

    let c_env_ptrs: Vec<*const libc::c_char> = c_env
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // SAFETY: execve with valid C strings and null-terminated arrays.
    unsafe { libc::execve(c_cmd.as_ptr(), c_args_ptrs.as_ptr(), c_env_ptrs.as_ptr()) };

    Err(JailError::Exec(std::io::Error::last_os_error()))
}

/// Set RLIMIT_NOFILE to prevent child from exhausting system-wide fds.
fn set_fd_limit(max: u64) {
    let rlim = libc::rlimit {
        rlim_cur: max,
        rlim_max: max,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) };
}

/// Set RLIMIT_CORE to prevent core dumps (sensitive memory to output dir).
fn set_core_limit(max: u64) {
    let rlim = libc::rlimit {
        rlim_cur: max,
        rlim_max: max,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_CORE, &rlim) };
}

/// Drop every capability from the ambient, bounding, and
/// effective/permitted/inheritable sets, then lock down securebits
/// so uid-0 stops implying caps and setuid transitions can't restore
/// them. Called after PR_SET_NO_NEW_PRIVS and before seccomp.
///
/// Order matters: capset() strips CAP_SETPCAP, after which neither
/// PR_SET_SECUREBITS nor PR_CAPBSET_DROP will succeed. So we clear
/// ambient → lock securebits → drop bounding → zero the sets last.
fn drop_all_capabilities() {
    // Ambient: single call clears all 64 caps.
    // SAFETY: prctl with scalar args is always safe.
    unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
            0,
            0,
            0,
        );
    }

    // Securebits — uid 0 no longer means "has all caps", and setuid
    // transitions don't re-grant caps. The *_LOCKED pair freezes
    // these bits so nothing later can undo them.
    const SECBIT_NOROOT: libc::c_ulong = 1 << 0;
    const SECBIT_NOROOT_LOCKED: libc::c_ulong = 1 << 1;
    const SECBIT_NO_SETUID_FIXUP: libc::c_ulong = 1 << 2;
    const SECBIT_NO_SETUID_FIXUP_LOCKED: libc::c_ulong = 1 << 3;
    // SAFETY: prctl with scalar args.
    unsafe {
        libc::prctl(
            libc::PR_SET_SECUREBITS,
            SECBIT_NOROOT
                | SECBIT_NOROOT_LOCKED
                | SECBIT_NO_SETUID_FIXUP
                | SECBIT_NO_SETUID_FIXUP_LOCKED,
            0,
            0,
            0,
        );
    }

    // Bounding set — 63 is a safe upper bound; EINVAL on unknown
    // cap numbers is fine, the loop continues.
    for cap in 0..=63 {
        // SAFETY: prctl with scalar args; errors ignored by design.
        unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) };
    }

    // Effective / permitted / inheritable — zero all three. Use the
    // raw syscall with _LINUX_CAPABILITY_VERSION_3 (64-bit caps, two
    // data blocks).
    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let hdr = CapHeader { version: 0x20080522, pid: 0 };
    let data: [CapData; 2] = [
        CapData { effective: 0, permitted: 0, inheritable: 0 },
        CapData { effective: 0, permitted: 0, inheritable: 0 },
    ];
    // SAFETY: hdr and data live on the stack for the whole call;
    // pointers are valid and correctly aligned for the kernel ABI.
    unsafe {
        libc::syscall(libc::SYS_capset, &hdr as *const _, data.as_ptr());
    }
}

/// Prepend each flavor's `bin/` directory to the jail's `PATH` so
/// binaries shipped with an overlay are discoverable without the caller
/// having to know anything about the flavor's layout.
///
/// Only entries that actually contain a `bin/` subdirectory are added —
/// a flavor that ships libs + headers only won't pollute `PATH`.
fn prepend_flavor_bins_to_path(env: &mut Vec<(String, String)>, overlays: &[std::path::PathBuf]) {
    let mut bins: Vec<String> = Vec::new();
    for overlay in overlays {
        let Some(name) = overlay.file_name().and_then(|n| n.to_str()) else { continue };
        if overlay.join("bin").is_dir() {
            bins.push(format!("/opt/flavors/{name}/bin"));
        }
    }
    if bins.is_empty() { return; }

    let joined = bins.join(":");
    match env.iter_mut().find(|(k, _)| k == "PATH") {
        Some((_, v)) if v.is_empty() => *v = joined,
        Some((_, v))                 => *v = format!("{joined}:{v}"),
        None => env.push(("PATH".into(), joined)),
    }
}

/// Mark every inherited file descriptor above stdio as CLOEXEC so
/// it is closed at execve. Agentjail's own fds are all already
/// CLOEXEC; this neutralizes third-party libraries (OpenSSL, CUDA,
/// Python extensions) that leak descriptors into the embedding
/// process.
fn close_inherited_fds() {
    // Flag from linux/close_range.h — mark range CLOEXEC rather
    // than closing now, so we don't disturb anything agentjail is
    // still using during setup.
    const CLOSE_RANGE_CLOEXEC: libc::c_uint = 4;
    // SAFETY: close_range with scalar args. If the kernel is <5.11
    // or the CLOEXEC flag is unsupported (pre-5.11 did not have
    // it), the syscall errors out silently — agentjail's own
    // CLOEXEC hygiene still holds, so nothing regresses.
    unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            CLOSE_RANGE_CLOEXEC,
        );
    }
}
