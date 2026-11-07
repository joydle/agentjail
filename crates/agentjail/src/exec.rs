//! Child-side jail setup: runs post-fork, pre-exec.
//!
//! # Fork safety
//!
//! This code runs in a child forked from a (usually multi-threaded,
//! tokio) process. Only the forking thread exists in the child, and any
//! lock another thread held at fork time stays locked forever. So:
//!
//! - argv/envp are built by the parent *before* fork ([`ExecPlan`]);
//! - diagnostics never go through `eprintln!`/`format!` (the stderr lock
//!   and `Display` impls); failures write a static message straight to
//!   fd 2 and `_exit(127)` ([`fail`]);
//! - every hardening call is checked, and the sandbox fails closed;
//! - a panic is caught by the caller (`catch_unwind` in `run.rs`) and
//!   turned into `_exit(127)`, so it never unwinds into the duplicated
//!   copy of the embedder.
//!
//! The remaining allocations (path joins in the mount helpers, netlink
//! buffers) rely on glibc's fork-aware malloc, which resets its arena
//! locks in the child.
//!
//! # Order
//!
//! ```text
//! userns handshake → unshare(mnt,net,ipc) → network → jail root + binds
//!   → rlimits, no_new_privs, CLOEXEC sweep
//!   → [pid ns: unshare(NEWPID) + fork; the jail's init continues]
//!   → mount /proc + mask → pivot_root → Landlock → drop privileges
//!   → seccomp → execve
//! ```
//!
//! `/proc` is mounted by the jail's init *before* `pivot_root`: with a
//! user namespace the kernel only allows a procfs mount while a fully
//! visible procfs (the host's) is still in the mount namespace. Landlock
//! comes after every mount operation, because a landlocked process may
//! no longer change its mount topology.

use crate::config::{JailConfig, Network};
use crate::error::{JailError, Result};
use crate::namespace::{
    NamespaceConfig, enter_namespaces, enter_user_namespace_alone, setup_loopback,
};
use crate::seccomp::{CompiledFilter, apply_compiled};
use crate::{gpu, landlock, mount, netlink};

use std::ffi::CString;
use std::net::Ipv4Addr;
use std::os::fd::RawFd;
use std::path::Path;
use std::time::Duration;

/// How long the child waits for each parent reply during setup.
pub(crate) const CHILD_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// argv/envp for `execve`, built in the parent before fork so the child
/// never has to allocate or format them.
pub(crate) struct ExecPlan {
    cmd: CString,
    _argv: Vec<CString>,
    _envp: Vec<CString>,
    argv_ptrs: Vec<*const libc::c_char>,
    envp_ptrs: Vec<*const libc::c_char>,
}

impl ExecPlan {
    pub(crate) fn new(cmd: &str, args: &[&str], env: &[(String, String)]) -> Result<Self> {
        let bad = |what: &str| {
            JailError::Exec(std::io::Error::other(format!("{what} contains a NUL byte")))
        };
        let c_cmd = CString::new(cmd).map_err(|_| bad("command"))?;
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(c_cmd.clone());
        for a in args {
            argv.push(CString::new(*a).map_err(|_| bad("argument"))?);
        }
        let mut envp = Vec::with_capacity(env.len());
        for (k, v) in env {
            envp.push(CString::new(format!("{k}={v}")).map_err(|_| bad("environment variable"))?);
        }
        // The CString heap buffers don't move when the Vec does, so the
        // pointer arrays stay valid for the plan's lifetime.
        let argv_ptrs = argv
            .iter()
            .map(|s| s.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        let envp_ptrs = envp
            .iter()
            .map(|s| s.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        Ok(Self {
            cmd: c_cmd,
            _argv: argv,
            _envp: envp,
            argv_ptrs,
            envp_ptrs,
        })
    }
}

/// Veth parameters for `Network::Allowlist`, allocated by the parent
/// before fork.
pub(crate) struct VethChild {
    pub jail_if: String,
    pub host_ip: Ipv4Addr,
    pub jail_ip: Ipv4Addr,
}

/// Everything the child needs, prepared by the parent.
pub(crate) struct ChildCtx<'a> {
    pub config: &'a JailConfig,
    pub gpu: Option<&'a gpu::NvidiaResources>,
    pub seccomp: Option<&'a CompiledFilter>,
    pub plan: &'a ExecPlan,
    /// Child end of the userns handshake socket (`user_namespace`).
    pub userns_fd: Option<RawFd>,
    /// Child end of the netns handshake socket (`Network::Allowlist`).
    pub netns_fd: Option<RawFd>,
    pub veth: Option<&'a VethChild>,
    /// The embedder is real root: clear supplementary groups before
    /// unsharing the user namespace.
    pub embedder_is_root: bool,
    /// Host uid/gid the jail's init switches to before exec
    /// (`user_namespace = false` with a root embedder).
    pub run_as: Option<(u32, u32)>,
    /// Where to create the pre-pivot root (see `mount::jail_root_path`).
    pub jail_root: &'a Path,
}

// --- Failure reporting (no allocation, no locks) ---------------------------

fn write_stderr(bytes: &[u8]) {
    // SAFETY: write(2) on fd 2 with a valid buffer; errors are ignored
    // because there is nowhere left to report them.
    unsafe { libc::write(2, bytes.as_ptr().cast(), bytes.len()) };
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Report `stage` (and `err` if non-zero) on fd 2, then `_exit(127)`.
pub(crate) fn fail(stage: &'static str, err: i32) -> ! {
    write_stderr(b"agentjail: jail setup failed: ");
    write_stderr(stage.as_bytes());
    if err != 0 {
        let mut buf = [0u8; 12];
        let mut i = buf.len();
        let mut n = err.unsigned_abs();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        write_stderr(b" (errno ");
        write_stderr(&buf[i..]);
        write_stderr(b")");
    }
    write_stderr(b"\n");
    // SAFETY: _exit is async-signal-safe and never returns.
    unsafe { libc::_exit(127) }
}

fn errno_of(e: &JailError) -> i32 {
    match e {
        JailError::Namespace(e) | JailError::Mount(e) | JailError::Fork(e) | JailError::Pipe(e) => {
            e.raw_os_error()
        }
        JailError::UidMap(e)
        | JailError::Io(e)
        | JailError::Cgroup(e)
        | JailError::Snapshot(e)
        | JailError::Gpu(e)
        | JailError::Network(e)
        | JailError::Exec(e) => e.raw_os_error().unwrap_or(0),
        JailError::PathNotFound(_) => libc::ENOENT,
        _ => 0,
    }
}

fn stage<T>(r: Result<T>, what: &'static str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => fail(what, errno_of(&e)),
    }
}

/// `ret == 0` or fail closed.
fn check(ret: libc::c_long, what: &'static str) {
    if ret != 0 {
        fail(what, errno());
    }
}

// --- Handshake helpers -------------------------------------------------------

/// Send one byte on a socketpair end (`MSG_NOSIGNAL`: a dead peer is an
/// error, not a SIGPIPE).
pub(crate) fn send_byte(fd: RawFd) -> bool {
    // SAFETY: valid fd, one-byte buffer on the stack.
    unsafe { libc::send(fd, [1u8].as_ptr().cast(), 1, libc::MSG_NOSIGNAL) == 1 }
}

/// Outcome of [`recv_byte`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Recv {
    Ok,
    /// Peer closed its end (died) before sending.
    Closed,
    TimedOut,
    Error(i32),
}

/// Wait up to `timeout` for one byte on `fd`. Allocation-free; usable
/// on both sides of the fork.
pub(crate) fn recv_byte(fd: RawFd, timeout: Duration) -> Recv {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Recv::TimedOut;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        // SAFETY: one valid pollfd on the stack.
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n < 0 {
            let e = errno();
            if e == libc::EINTR {
                continue;
            }
            return Recv::Error(e);
        }
        if n == 0 {
            continue; // loop re-checks the deadline
        }
        let mut b = [0u8; 1];
        // SAFETY: valid fd, one-byte buffer on the stack.
        let r = unsafe { libc::recv(fd, b.as_mut_ptr().cast(), 1, 0) };
        return match r {
            1 => Recv::Ok,
            0 => Recv::Closed,
            _ => {
                let e = errno();
                if e == libc::EINTR || e == libc::EAGAIN {
                    continue;
                }
                Recv::Error(e)
            }
        };
    }
}

fn child_recv(fd: RawFd, what: &'static str) {
    match recv_byte(fd, CHILD_SYNC_TIMEOUT) {
        Recv::Ok => {}
        Recv::Error(e) => fail(what, e),
        Recv::Closed | Recv::TimedOut => fail(what, 0),
    }
}

// --- Child entry -------------------------------------------------------------

/// Set up the jail and exec the command. Never returns.
pub(crate) fn run_child(ctx: &ChildCtx<'_>) -> ! {
    let config = ctx.config;

    // 0. User namespace first, with the parent writing uid_map/gid_map
    //    in between. The remaining unshares happen afterwards so they
    //    run under the new namespace's capabilities.
    if config.user_namespace {
        let Some(fd) = ctx.userns_fd else {
            fail("userns handshake fd missing", 0)
        };
        if ctx.embedder_is_root {
            // Drop host root's supplementary groups while we still can:
            // `setgroups` is denied inside the namespace.
            // SAFETY: setgroups(0, NULL) clears the group list.
            check(
                unsafe { libc::setgroups(0, std::ptr::null()) } as libc::c_long,
                "setgroups",
            );
        }
        stage(enter_user_namespace_alone(), "unshare(CLONE_NEWUSER)");
        if !send_byte(fd) {
            fail("userns handshake (send)", errno());
        }
        child_recv(fd, "userns handshake (parent did not write uid/gid maps)");
        // Become the namespace's root, i.e. the unprivileged host id the
        // parent mapped. Until now our credentials were still the
        // embedder's (host root for a root embedder).
        let ppid = unsafe { libc::getppid() };
        // SAFETY: plain syscalls with scalar args.
        check(
            unsafe { libc::setresgid(0, 0, 0) } as libc::c_long,
            "setresgid(0) in userns",
        );
        check(
            unsafe { libc::setresuid(0, 0, 0) } as libc::c_long,
            "setresuid(0) in userns",
        );
        rearm_pdeathsig(ppid);
        // SAFETY: fd is ours; done with it.
        unsafe { libc::close(fd) };
    }

    // 1. Remaining namespaces (pid via double-fork below).
    let ns_config = NamespaceConfig {
        user: false,
        mount: true,
        pid: false,
        network: true,
        ipc: config.ipc_namespace,
    };
    stage(enter_namespaces(ns_config), "unshare(mnt/net/ipc)");

    // 2. Network.
    match &config.network {
        Network::None => {}
        Network::Loopback => stage(setup_loopback(), "loopback up"),
        Network::Allowlist(_) => {
            let (Some(fd), Some(veth)) = (ctx.netns_fd, ctx.veth) else {
                fail("netns handshake fd missing", 0)
            };
            if !send_byte(fd) {
                fail("netns handshake (send)", errno());
            }
            child_recv(fd, "netns handshake (parent did not set up the veth)");
            // SAFETY: fd is ours; done with it.
            unsafe { libc::close(fd) };
            stage(setup_loopback(), "loopback up");
            stage(
                netlink::add_ipv4_addr(&veth.jail_if, veth.jail_ip, 30),
                "veth address",
            );
            stage(netlink::set_link_up(&veth.jail_if), "veth up");
            stage(netlink::add_default_route(veth.host_ip), "default route");
        }
    }

    // 3. Filesystem.
    stage(mount::make_root_private(), "make / private");
    // Unpredictable temp dir so a host-side attacker can't race a
    // symlink into place at a guessed path before setup_root runs.
    stage(mount::make_jail_root(ctx.jail_root), "create jail root");
    let new_root = ctx.jail_root;
    stage(
        mount::setup_root(
            new_root,
            &config.source,
            &config.output,
            config.source_rw,
            &config.readonly_overlays,
        ),
        "mount jail root",
    );
    if let Some(res) = ctx.gpu {
        stage(gpu::setup_mounts(new_root, res), "GPU mounts");
    }

    // 4. Process-wide hardening, inherited by the jail's init.
    // Mark every inherited fd CLOEXEC (before lowering RLIMIT_NOFILE so
    // the fallback sweep still covers high fds).
    close_inherited_fds();
    set_rlimit(libc::RLIMIT_NOFILE, 4096, "setrlimit(RLIMIT_NOFILE)");
    // No core dumps: they could write sensitive memory to the output dir.
    set_rlimit(libc::RLIMIT_CORE, 0, "setrlimit(RLIMIT_CORE)");
    // Setuid binaries can't elevate, even with seccomp disabled. Also a
    // precondition for Landlock and seccomp without CAP_SYS_ADMIN.
    // SAFETY: prctl with scalar args.
    check(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } as libc::c_long,
        "prctl(PR_SET_NO_NEW_PRIVS)",
    );

    // 5. PID namespace double-fork. Capabilities are dropped only in
    //    the jail's init, after the procfs mount and pivot (both need
    //    CAP_SYS_ADMIN).
    if config.pid_namespace {
        enter_pid_namespace();
    }

    // 6. The jail's init (or the only process without a pid ns).
    finish_and_exec(ctx, new_root)
}

/// `unshare(NEWPID)` + fork. Returns in the child (pid 1 of the new
/// namespace); the intermediate process waits and mirrors its exit
/// status.
fn enter_pid_namespace() {
    use rustix::thread::{UnshareFlags, unshare};

    stage(
        unshare(UnshareFlags::NEWPID).map_err(JailError::Namespace),
        "unshare(CLONE_NEWPID)",
    );

    // SAFETY: fork() in an already-forked, single-threaded child. The
    // grandchild only runs the same restricted setup code and then
    // execs or `_exit`s.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => fail("fork (pid namespace)", errno()),
        0 => {
            // Re-arm parent-death signal (cleared by fork), keeping the
            // chain parent → child → grandchild linked.
            // SAFETY: prctl with scalar args.
            check(
                unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } as libc::c_long,
                "prctl(PR_SET_PDEATHSIG)",
            );
        }
        child_pid => {
            let mut status: libc::c_int = 0;
            loop {
                // SAFETY: waiting for our own child with a valid pointer.
                let r = unsafe { libc::waitpid(child_pid, &mut status, 0) };
                if r == child_pid {
                    break;
                }
                if r < 0 && errno() != libc::EINTR {
                    // SAFETY: nothing left to do; report a generic failure.
                    unsafe { libc::_exit(1) };
                }
            }
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                1
            };
            // SAFETY: exiting the intermediate process.
            unsafe { libc::_exit(code) };
        }
    }
}

fn finish_and_exec(ctx: &ChildCtx<'_>, new_root: &Path) -> ! {
    let config = ctx.config;

    // Fresh procfs for this pid namespace, then mask it. Must succeed:
    // a missing/unmasked /proc is either a host-pid leak or a host-root
    // write primitive (core_pattern, sysrq-trigger).
    let proc_dir = new_root.join("proc");
    stage(mount::mount_proc(&proc_dir), "mount /proc");
    stage(mount::mask_proc(&proc_dir), "mask /proc");

    // pivot_root + MNT_DETACH fully removes the host filesystem from the
    // mount namespace — a plain chroot(2) leaves the old root attached
    // and is escapable via nested-chroot + fchdir + "..".
    stage(mount::seal_root(new_root), "read-only jail root");
    stage(mount::pivot_into(new_root), "pivot_root");
    // SAFETY: static C string.
    check(
        unsafe { libc::chdir(c"/workspace".as_ptr()) } as libc::c_long,
        "chdir /workspace",
    );

    if config.landlock {
        stage(apply_landlock(config), "landlock");
    }

    let ppid = unsafe { libc::getppid() };
    drop_privileges(ctx.run_as);
    rearm_pdeathsig(ppid);

    // Seccomp last before exec. The filter was compiled in `Jail::new`.
    if let Some(bpf) = ctx.seccomp {
        stage(apply_compiled(bpf), "seccomp");
    }

    let plan = ctx.plan;
    // SAFETY: execve with NUL-terminated C strings and NULL-terminated
    // pointer arrays owned by `plan`.
    unsafe {
        libc::execve(
            plan.cmd.as_ptr(),
            plan.argv_ptrs.as_ptr(),
            plan.envp_ptrs.as_ptr(),
        )
    };
    fail("execve", errno())
}

/// Landlock ruleset over the in-jail paths. Applied after `pivot_root`
/// and the procfs mount (see module docs).
fn apply_landlock(config: &JailConfig) -> Result<()> {
    let source = if config.source_rw {
        landlock::read_write() | landlock::read_exec()
    } else {
        landlock::read_exec()
    };
    let mut rules = vec![
        (Path::new("/bin"), landlock::read_exec()),
        (Path::new("/lib"), landlock::read_exec()),
        (Path::new("/lib64"), landlock::read_exec()),
        (Path::new("/usr"), landlock::read_exec()),
        (Path::new("/etc"), landlock::read_only()),
        (Path::new("/proc"), landlock::read_only()),
        (Path::new("/dev"), landlock::devices()),
        (Path::new("/tmp"), landlock::read_write()),
        (
            Path::new("/output"),
            landlock::read_write() | landlock::read_exec(),
        ),
        (Path::new("/workspace"), source),
    ];
    if !config.readonly_overlays.is_empty() {
        rules.push((Path::new("/opt/flavors"), landlock::read_exec()));
    }
    landlock::restrict(&rules)
}

#[cfg(target_env = "gnu")]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
type RlimitResource = libc::c_int;

fn set_rlimit(resource: RlimitResource, max: u64, what: &'static str) {
    let rlim = libc::rlimit {
        rlim_cur: max,
        rlim_max: max,
    };
    // SAFETY: valid pointer to a stack rlimit.
    check(
        unsafe { libc::setrlimit(resource, &rlim) } as libc::c_long,
        what,
    );
}

/// Drop every capability from the ambient, bounding, and
/// effective/permitted/inheritable sets, lock down securebits so uid 0
/// stops implying caps, and (root embedder without a user namespace)
/// switch to the configured unprivileged uid/gid. Every step is checked;
/// any failure kills the child.
///
/// Order matters: capset() strips CAP_SETPCAP/CAP_SETUID, after which
/// neither PR_SET_SECUREBITS, PR_CAPBSET_DROP nor setresuid would
/// succeed. So: ambient → securebits → bounding → ids → capset.
/// Re-arm `PR_SET_PDEATHSIG(SIGKILL)` after a credential change.
///
/// The kernel clears the parent-death signal whenever the effective or
/// filesystem uid/gid changes (`commit_creds`), e.g. `setresuid(0)` in a
/// user namespace mapped to an unprivileged host id, or the `run_as`
/// drop. Without this, a root embedder's jail would survive the
/// embedder. `ppid` is `getppid()` from before the change: if it moved,
/// the parent died in between and we were reparented. (pid 1 of a new
/// pid namespace always sees 0 and can't tell; its parent, the setup
/// process, is armed, and `kill_tree` reaches the whole session.)
fn rearm_pdeathsig(ppid: libc::pid_t) {
    // SAFETY: prctl / getppid with scalar args.
    check(
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } as libc::c_long,
        "prctl(PR_SET_PDEATHSIG) after credential change",
    );
    if unsafe { libc::getppid() } != ppid {
        // SAFETY: the parent is gone; leave without running anything.
        unsafe { libc::_exit(127) };
    }
}

fn drop_privileges(run_as: Option<(u32, u32)>) {
    // SAFETY (whole fn): prctl / set*id / capset with scalar args or
    // pointers to stack values that outlive the call.
    check(
        unsafe {
            libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
                0,
                0,
                0,
            )
        } as libc::c_long,
        "prctl(PR_CAP_AMBIENT_CLEAR_ALL)",
    );

    // Securebits — uid 0 no longer means "has all caps", and setuid
    // transitions don't re-grant caps. The *_LOCKED pair freezes these
    // bits so nothing later can undo them.
    const SECBIT_NOROOT: libc::c_ulong = 1 << 0;
    const SECBIT_NOROOT_LOCKED: libc::c_ulong = 1 << 1;
    const SECBIT_NO_SETUID_FIXUP: libc::c_ulong = 1 << 2;
    const SECBIT_NO_SETUID_FIXUP_LOCKED: libc::c_ulong = 1 << 3;
    check(
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
            )
        } as libc::c_long,
        "prctl(PR_SET_SECUREBITS)",
    );

    // Bounding set: drop until the kernel says EINVAL (past CAP_LAST_CAP).
    for cap in 0..64 {
        if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) } != 0 {
            let e = errno();
            if e == libc::EINVAL && cap > 0 {
                break;
            }
            fail("prctl(PR_CAPBSET_DROP)", e);
        }
    }

    if let Some((uid, gid)) = run_as {
        check(
            unsafe { libc::setgroups(0, std::ptr::null()) } as libc::c_long,
            "setgroups",
        );
        check(
            unsafe { libc::setresgid(gid, gid, gid) } as libc::c_long,
            "setresgid(run_as)",
        );
        check(
            unsafe { libc::setresuid(uid, uid, uid) } as libc::c_long,
            "setresuid(run_as)",
        );
        if unsafe { libc::getuid() != uid || libc::geteuid() != uid || libc::getgid() != gid } {
            fail("run_as uid/gid did not stick", 0);
        }
    }

    // Effective / permitted / inheritable — zero all three
    // (_LINUX_CAPABILITY_VERSION_3: 64-bit caps, two data blocks).
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
    let hdr = CapHeader {
        version: 0x2008_0522,
        pid: 0,
    };
    let data: [CapData; 2] = [
        CapData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
        CapData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    check(
        unsafe { libc::syscall(libc::SYS_capset, &hdr as *const CapHeader, data.as_ptr()) },
        "capset",
    );

    // Belt and braces: CAP_SYS_ADMIN must be gone from the bounding set.
    const CAP_SYS_ADMIN: libc::c_ulong = 21;
    if unsafe { libc::prctl(libc::PR_CAPBSET_READ, CAP_SYS_ADMIN, 0, 0, 0) } != 0 {
        fail("capability bounding set still holds CAP_SYS_ADMIN", 0);
    }
}

/// Mark every inherited file descriptor above stdio as CLOEXEC so it is
/// closed at execve. Agentjail's own fds are all already CLOEXEC; this
/// neutralizes third-party libraries (OpenSSL, CUDA, Python extensions)
/// that leak descriptors into the embedding process.
fn close_inherited_fds() {
    // Flag from linux/close_range.h — mark the range CLOEXEC rather than
    // closing now, so we don't disturb fds the setup still uses.
    const CLOSE_RANGE_CLOEXEC: libc::c_uint = 4;
    // SAFETY: close_range with scalar args.
    let r = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOSE_RANGE_CLOEXEC) };
    if r == 0 {
        return;
    }
    // Pre-5.11 kernel: sweep up to the current soft limit by hand.
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid pointer to a stack rlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) } != 0 {
        fail("getrlimit(RLIMIT_NOFILE)", errno());
    }
    let max = rlim.rlim_cur.min(1 << 20) as libc::c_int;
    for fd in 3..max {
        // SAFETY: F_SETFD on an arbitrary fd; EBADF for closed fds is fine.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

/// Prepend each flavor's `bin/` directory to the jail's `PATH` so
/// binaries shipped with an overlay are discoverable without the caller
/// having to know anything about the flavor's layout.
///
/// Only entries that actually contain a `bin/` subdirectory are added —
/// a flavor that ships libs + headers only won't pollute `PATH`.
pub(crate) fn prepend_flavor_bins_to_path(
    env: &mut Vec<(String, String)>,
    overlays: &[std::path::PathBuf],
) {
    let mut bins: Vec<String> = Vec::new();
    for overlay in overlays {
        let Some(name) = overlay.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if overlay.join("bin").is_dir() {
            bins.push(format!("/opt/flavors/{name}/bin"));
        }
    }
    if bins.is_empty() {
        return;
    }

    let joined = bins.join(":");
    match env.iter_mut().find(|(k, _)| k == "PATH") {
        Some((_, v)) if v.is_empty() => *v = joined,
        Some((_, v)) => *v = format!("{joined}:{v}"),
        None => env.push(("PATH".into(), joined)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_plan_rejects_nul() {
        assert!(ExecPlan::new("/bin/sh\0", &[], &[]).is_err());
        assert!(ExecPlan::new("/bin/sh", &["a\0b"], &[]).is_err());
        assert!(ExecPlan::new("/bin/sh", &[], &[("K".into(), "v\0".into())]).is_err());
        let p = ExecPlan::new("/bin/sh", &["-c", "true"], &[("A".into(), "1".into())]).unwrap();
        assert_eq!(p.argv_ptrs.len(), 4);
        assert_eq!(p.envp_ptrs.len(), 2);
        assert!(p.argv_ptrs[3].is_null() && p.envp_ptrs[1].is_null());
    }

    #[test]
    fn recv_byte_reports_closed_peer_and_timeout() {
        let mut fds = [0; 2];
        // SAFETY: valid array for socketpair.
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        assert_eq!(recv_byte(fds[0], Duration::from_millis(50)), Recv::TimedOut);
        assert!(send_byte(fds[1]));
        assert_eq!(recv_byte(fds[0], Duration::from_millis(50)), Recv::Ok);
        // SAFETY: closing our own fd.
        unsafe { libc::close(fds[1]) };
        assert_eq!(recv_byte(fds[0], Duration::from_secs(5)), Recv::Closed);
        assert!(
            !send_byte(fds[0]),
            "send to a closed peer must fail, not SIGPIPE"
        );
        unsafe { libc::close(fds[0]) };
    }
}
