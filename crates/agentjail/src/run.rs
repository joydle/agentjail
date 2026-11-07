//! Jail execution: fork, setup, exec.

use crate::cgroup::Cgroup;
use crate::config::{JailConfig, Network};
use crate::error::{JailError, Result};
use crate::events::{EventReceiver, EventSender, JailEvent};
use crate::fork::{self, ForkInfo};
use crate::namespace::write_uid_gid_map;
use crate::pipe::{OutputStream, Pipe};
use crate::run_internal::{kill_tree, wait_for_pid};
use crate::veth::{VethPair, proxy_env_vars, spawn_proxy, sync_socketpair};
use crate::{events, exec, gpu};

use rustix::process::{Pid, WaitOptions, waitpid};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Process ID for a jailed process. Prevents accidentally mixing up PIDs
/// with file descriptors, signal numbers, or other integer types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JailPid(u32);

impl JailPid {
    /// Raw PID value for display/logging.
    #[must_use]
    pub fn as_raw(self) -> u32 {
        self.0
    }

    /// Convert to `i32` for syscalls (`kill`, `waitpid`, etc.).
    pub(crate) fn as_i32(self) -> i32 {
        self.0 as i32
    }

    /// Convert to rustix `Pid` for `waitpid`.
    pub(crate) fn to_rustix(self) -> Option<Pid> {
        Pid::from_raw(self.as_i32())
    }
}

impl std::fmt::Display for JailPid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A configured jail ready to execute commands.
///
/// The config, GPU resources, and compiled seccomp filter are all
/// shared by reference across every `spawn()`. Each field is stored
/// in an `Arc` so spawning N jails from one `Jail` does not clone
/// the underlying `Vec<String>` allowlist or the env table.
pub struct Jail {
    config: Arc<JailConfig>,
    /// Pre-discovered GPU resources (if gpu.enabled).
    gpu_resources: Option<Arc<gpu::NvidiaResources>>,
    /// Pre-compiled seccomp BPF — shared by every spawn, compiled once.
    seccomp_filter: Option<Arc<crate::seccomp::CompiledFilter>>,
}

/// Handle to a running jailed process.
pub struct JailHandle {
    pid: JailPid,
    /// Set to true after the child has been waited on. Prevents Drop from
    /// killing a recycled PID at high concurrency.
    reaped: bool,
    pub stdout: OutputStream,
    pub stderr: OutputStream,
    start_time: Instant,
    timeout: Duration,
    cgroup: Option<Cgroup>,
    /// The jail's veth pair (Allowlist mode only). Dropping it deletes
    /// the link and releases its id.
    veth: Option<VethPair>,
    /// Jail-side IPv4 address on the veth pair (`Some` only when the
    /// jail was spawned with `Network::Allowlist`). The host can reach
    /// this address directly for inbound forwarding.
    jail_ip: Option<std::net::Ipv4Addr>,
    /// Shutdown signal for the proxy thread (Allowlist mode only).
    proxy_shutdown: Option<tokio::sync::watch::Sender<bool>>,
    /// Event sender installed by [`Jail::spawn_with_events`]; `wait`
    /// streams through it.
    events: Option<EventSender>,
    /// The jail's pre-pivot root directory, removed with the handle.
    _jail_root: JailRoot,
}

/// The host-side directory a jail's root was built in. After the jail
/// pivots it is an empty directory in the host's view (the mounts live
/// in the jail's mount namespace), so it is removed once the jail is
/// gone: on spawn failure, or when the handle is dropped after reaping.
struct JailRoot(PathBuf);

impl Drop for JailRoot {
    fn drop(&mut self) {
        // `remove_dir` only removes an empty directory, and never
        // follows a symlink.
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// Result of a completed jail execution.
#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub duration: Duration,
    pub timed_out: bool,
    pub oom_killed: bool,
    pub stats: Option<ResourceStats>,
}

/// Resource usage statistics from cgroup.
#[derive(Debug, Clone, Default)]
pub struct ResourceStats {
    /// Peak memory usage in bytes (watermark — never decreases).
    pub memory_peak_bytes: u64,
    /// Current memory usage in bytes at sample time.
    pub memory_current_bytes: u64,
    /// Total CPU time used in microseconds.
    pub cpu_usage_usec: u64,
    /// Whether OOM killer was triggered.
    pub oom_killed: bool,
    /// Total bytes read from disk.
    pub io_read_bytes: u64,
    /// Total bytes written to disk.
    pub io_write_bytes: u64,
    /// Processes/threads currently alive inside the jail.
    pub pids_current: u64,
}

/// How long the parent waits for each child signal during the spawn
/// handshake. A child that dies closes its socket ends and is noticed
/// immediately; the deadline only bounds a child that hangs.
const SPAWN_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on stdout/stderr bytes buffered by [`JailHandle::wait_with_events`]
/// (same as `OutputStream::read_all`).
const MAX_EVENT_OUTPUT_BYTES: usize = 256 * 1024 * 1024;

/// Per-spawn slot counter for `userns_id_count > 1`.
static NEXT_USERNS_SLOT: AtomicU32 = AtomicU32::new(0);

/// Test hooks: make the forked child misbehave before the handshake.
#[cfg(test)]
pub(crate) static TEST_CHILD_MODE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
pub(crate) const TEST_CHILD_EXIT_EARLY: u8 = 1;
#[cfg(test)]
pub(crate) const TEST_CHILD_PANIC: u8 = 2;

/// Parent side: wait for the child's one-byte signal for `step`.
fn parent_recv(fd: &OwnedFd, step: &str) -> Result<()> {
    match exec::recv_byte(fd.as_raw_fd(), SPAWN_SYNC_TIMEOUT) {
        exec::Recv::Ok => Ok(()),
        exec::Recv::Closed => Err(JailError::Handshake(format!(
            "child exited before {step} (see the jail's stderr)"
        ))),
        exec::Recv::TimedOut => Err(JailError::Handshake(format!(
            "child did not reach {step} within {SPAWN_SYNC_TIMEOUT:?}"
        ))),
        exec::Recv::Error(e) => Err(JailError::Handshake(format!(
            "{step}: {}",
            std::io::Error::from_raw_os_error(e)
        ))),
    }
}

/// Parent side: release the child past `step`.
fn parent_send(fd: &OwnedFd, step: &str) -> Result<()> {
    if exec::send_byte(fd.as_raw_fd()) {
        Ok(())
    } else {
        Err(JailError::Handshake(format!(
            "child exited before {step}: {}",
            std::io::Error::last_os_error()
        )))
    }
}

impl Jail {
    /// Create a new jail from configuration.
    ///
    /// Validates the config and paths and discovers GPU resources upfront
    /// so errors are reported before forking.
    pub fn new(config: JailConfig) -> Result<Self> {
        config.validate()?;
        if !config.source.exists() {
            return Err(JailError::PathNotFound(config.source.clone()));
        }
        if !config.output.exists() {
            return Err(JailError::PathNotFound(config.output.clone()));
        }
        if config.landlock && !crate::landlock::is_available() {
            // The config asked for Landlock but the kernel can't provide
            // it (<5.13 or disabled). Soft-falling through would hand back
            // a jail without the FS defence the caller requested.
            return Err(JailError::Unsupported(
                "landlock = true but the kernel lacks Landlock \
                 (need Linux >= 5.13 with CONFIG_SECURITY_LANDLOCK enabled); \
                 set landlock = false to accept the weaker posture"
                    .into(),
            ));
        }

        let gpu_resources = if config.gpu.enabled {
            Some(Arc::new(gpu::discover(&config.gpu)?))
        } else {
            None
        };

        // Compile the seccomp filter once. `apply_compiled` in the
        // child is then zero-allocation — no BPF rebuild per spawn.
        let seccomp_filter = crate::seccomp::compile(config.seccomp)?.map(Arc::new);

        Ok(Self {
            config: Arc::new(config),
            gpu_resources,
            seccomp_filter,
        })
    }

    /// Create cgroup for a new spawn.
    fn create_cgroup(&self, pid: JailPid) -> Result<Option<Cgroup>> {
        let config = &self.config;
        let memory_bytes = config.memory_limit_bytes()?;
        let (read_bps, write_bps) = config.io_limit_bps()?;
        let has_limits = memory_bytes.is_some()
            || config.cpu_percent > 0
            || config.max_pids > 0
            || read_bps > 0
            || write_bps > 0;

        if !has_limits {
            return Ok(None);
        }

        let name = format!("{}-{}", std::process::id(), pid.as_raw());
        let cg = Cgroup::create(&name)?;

        if let Some(bytes) = memory_bytes {
            cg.set_memory_limit(bytes)?;
        }
        if config.cpu_percent > 0 {
            cg.set_cpu_quota(config.cpu_percent)?;
        }
        if config.max_pids > 0 {
            cg.set_pids_max(config.max_pids)?;
        }
        if read_bps > 0 || write_bps > 0 {
            // Skip the I/O limit silently on a non-UTF-8 output path —
            // the previous fallback to `"/"` would have clamped the
            // whole root device, which is catastrophically wrong.
            match config.output.to_str() {
                Some(dev_path) => {
                    if let Err(e) = cg.set_io_limit(dev_path, read_bps, write_bps) {
                        eprintln!("warning: I/O limits not applied: {e}");
                    }
                }
                None => {
                    eprintln!(
                        "warning: I/O limits not applied: output path is not UTF-8: {}",
                        config.output.display()
                    );
                }
            }
        }

        Ok(Some(cg))
    }

    /// Host uid/gid the jail's files and processes will carry, and how
    /// the child gets there:
    /// - `map`: ids for the user-namespace uid/gid map (jail 0 → host);
    /// - `run_as`: ids the jail's init switches to (no user namespace).
    fn jail_ids(&self, embedder_is_root: bool) -> JailIds {
        let config = &self.config;
        if config.user_namespace {
            let ids = if embedder_is_root {
                let slot =
                    NEXT_USERNS_SLOT.fetch_add(1, Ordering::Relaxed) % config.userns_id_count;
                // `validate` guarantees base + count - 1 < u32::MAX.
                let id = config.userns_id_base + slot;
                (id, id)
            } else {
                (
                    rustix::process::getuid().as_raw(),
                    rustix::process::getgid().as_raw(),
                )
            };
            JailIds {
                host: ids,
                map: Some(ids),
                run_as: None,
            }
        } else if embedder_is_root {
            let ids = (config.run_as.uid, config.run_as.gid);
            JailIds {
                host: ids,
                map: None,
                run_as: Some(ids),
            }
        } else {
            let ids = (
                rustix::process::getuid().as_raw(),
                rustix::process::getgid().as_raw(),
            );
            JailIds {
                host: ids,
                map: None,
                run_as: None,
            }
        }
    }

    /// Spawn a command in the jail.
    ///
    /// Blocks until the child has finished the setup handshake (cgroup
    /// barrier, uid/gid maps, veth); it does not wait for the command.
    /// A child that dies during setup makes this return
    /// [`JailError::Handshake`] promptly.
    ///
    /// Safe to call from any thread: the fork runs on an internal
    /// process-lifetime thread ("agentjail-spawner"), so the jail's
    /// `PR_SET_PDEATHSIG` fires when the embedder process dies, not when
    /// the calling thread exits.
    pub fn spawn(&self, cmd: &str, args: &[&str]) -> Result<JailHandle> {
        let config = &*self.config;
        let embedder_is_root = rustix::process::geteuid().is_root();
        let ids = self.jail_ids(embedder_is_root);

        // A root embedder runs the jail as an unprivileged host id, so the
        // trees the jail must write (output, and source when `source_rw`)
        // are handed to that id. Without this, anything the embedder
        // seeded (or `live_fork` cloned) would be read-only to the jail.
        if embedder_is_root {
            let (uid, gid) = ids.host;
            chown_tree(&config.output, uid, gid).map_err(JailError::Io)?;
            if config.source_rw {
                chown_tree(&config.source, uid, gid).map_err(JailError::Io)?;
            }
        }

        // Allowlist: reserve the veth id up front so the child's env
        // (proxy URL) and interface names are known before fork. Nothing
        // exists on the host yet; dropping `veth` on any error below
        // deletes whatever was created and releases the id.
        let mut veth = match &config.network {
            Network::Allowlist(_) => Some(VethPair::allocate()?),
            _ => None,
        };
        let veth_child = veth.as_ref().map(|v| exec::VethChild {
            jail_if: v.jail_if().to_owned(),
            host_ip: v.host_ip(),
            jail_ip: v.jail_ip(),
        });

        // argv/envp are built here, not in the child (see exec.rs).
        let mut env = config.env.clone();
        if let Some(v) = &veth {
            env.extend(proxy_env_vars(v.host_ip()));
        }
        if self.gpu_resources.is_some() {
            env.extend(gpu::env_vars(&config.gpu));
        }
        if !config.readonly_overlays.is_empty() {
            exec::prepend_flavor_bins_to_path(&mut env, &config.readonly_overlays);
        }
        let plan = exec::ExecPlan::new(cmd, args, &env)?;

        let jail_root = JailRoot(crate::mount::jail_root_path()?);
        let stdout_pipe = Pipe::new()?;
        let stderr_pipe = Pipe::new()?;
        let stdin_null = if config.inherit_stdin {
            None
        } else {
            Some(
                std::fs::File::open("/dev/null")
                    .map(OwnedFd::from)
                    .map_err(JailError::Io)?,
            )
        };

        // Handshake sockets (child end, parent end). All CLOEXEC.
        // - barrier: child blocks until the parent assigned the cgroup;
        //   without it the child runs unconstrained during setup.
        // - userns: child unshares NEWUSER, signals; parent writes the
        //   maps against the *new* namespace and replies.
        // - netns (Allowlist): child signals "in my netns"; parent moves
        //   the veth in and replies.
        let barrier = sync_socketpair()?;
        let userns = if config.user_namespace {
            Some(sync_socketpair()?)
        } else {
            None
        };
        let netns = if veth.is_some() {
            Some(sync_socketpair()?)
        } else {
            None
        };

        let ctx = exec::ChildCtx {
            config,
            gpu: self.gpu_resources.as_deref(),
            seccomp: self.seccomp_filter.as_deref(),
            plan: &plan,
            userns_fd: userns.as_ref().map(|(c, _)| c.as_raw_fd()),
            netns_fd: netns.as_ref().map(|(c, _)| c.as_raw_fd()),
            veth: veth_child.as_ref(),
            embedder_is_root,
            run_as: ids.run_as,
            jail_root: &jail_root.0,
        };
        let child_fds = ChildFds {
            stdin: stdin_null.as_ref().map(|f| f.as_raw_fd()),
            stdout: stdout_pipe.write.as_raw_fd(),
            stderr: stderr_pipe.write.as_raw_fd(),
            barrier: barrier.0.as_raw_fd(),
            parent_ends: [
                Some(stdout_pipe.read.as_raw_fd()),
                Some(stderr_pipe.read.as_raw_fd()),
                Some(barrier.1.as_raw_fd()),
                userns.as_ref().map(|(_, p)| p.as_raw_fd()),
                netns.as_ref().map(|(_, p)| p.as_raw_fd()),
            ],
        };
        // SAFETY: getpid has no preconditions.
        let parent_pid = unsafe { libc::getpid() };

        // Fork on the process-lifetime spawner thread (see spawner.rs):
        // the child's PR_SET_PDEATHSIG follows the forking *thread*, and
        // the caller's thread may be short-lived. Everything the child
        // needs was built above; only the fork runs there, the rest of
        // the setup continues on this thread.
        let args = ForkArgs {
            ctx: &ctx,
            fds: &child_fds,
            parent_pid,
        };
        let forked = crate::spawner::run(move || {
            let args = args;
            // SAFETY: fork() in a multi-threaded process. The child must
            // only do fork-safe work: it never returns into the caller's
            // code (or the spawner's loop), never touches locks other
            // threads may hold (no eprintln!, no tokio), and leaves only
            // through execve or _exit (a panic is caught below). See
            // exec.rs for the details.
            match unsafe { libc::fork() } {
                -1 => Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0)),
                0 => {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        child_entry(args.ctx, args.fds, args.parent_pid)
                    }));
                    if r.is_err() {
                        // SAFETY: static buffer to fd 2, then _exit.
                        unsafe {
                            let msg = b"agentjail: jail setup panicked\n";
                            libc::write(2, msg.as_ptr().cast(), msg.len());
                            libc::_exit(127);
                        }
                    }
                    // child_entry never returns normally.
                    unsafe { libc::_exit(127) }
                }
                pid => Ok(pid),
            }
        })
        .map_err(JailError::Io)?;
        let child_pid = match forked {
            Ok(pid) => JailPid(pid as u32),
            Err(errno) => {
                return Err(JailError::Fork(rustix::io::Errno::from_raw_os_error(errno)));
            }
        };

        // Parent: close every child end right away, so a child that dies
        // makes our reads see EOF instead of blocking forever.
        drop(stdout_pipe.write);
        drop(stderr_pipe.write);
        drop(stdin_null);
        drop(barrier.0);
        let userns_parent = userns.map(|(c, p)| {
            drop(c);
            p
        });
        let netns_parent = netns.map(|(c, p)| {
            drop(c);
            p
        });

        // Guard: if anything below fails, kill and reap the child so it
        // doesn't become a zombie leaking PIDs, cgroups and veth interfaces.
        let child_guard = ChildGuard(child_pid);

        // 1. Cgroup, then release the barrier. The child blocks on the
        //    barrier *before* it does anything else, including the userns
        //    handshake, so this must come first.
        let cgroup = self.create_cgroup(child_pid)?;
        if let Some(ref cg) = cgroup {
            cg.add_pid(child_pid.as_raw())?;
        }
        parent_send(&barrier.1, "the cgroup barrier")?;
        drop(barrier.1);

        // 2. User namespace: child unshared NEWUSER → we write the maps
        //    for *that* namespace → release it.
        if let (Some(fd), Some((uid, gid))) = (&userns_parent, ids.map) {
            parent_recv(fd, "the user-namespace handshake")?;
            let pid = child_pid
                .to_rustix()
                .ok_or_else(|| JailError::Handshake("invalid child pid".into()))?;
            write_uid_gid_map(pid, uid, gid)?;
            parent_send(fd, "the uid/gid map acknowledgement")?;
        }

        // 3. Allowlist: wait for the child's netns, then veth, host
        //    firewall and proxy. Every step is fallible and fails the
        //    spawn: `?` drops (in order) the proxy sender, the child guard
        //    (kill + reap) and `veth` (delete the link, release the id).
        let mut proxy_shutdown = None;
        let mut jail_ip: Option<std::net::Ipv4Addr> = None;
        if let (Some(fd), Some(v), Network::Allowlist(domains)) =
            (&netns_parent, veth.as_mut(), &config.network)
        {
            parent_recv(fd, "the network-namespace handshake")?;

            v.create()?;
            v.attach(child_pid.as_raw())?;
            crate::veth::ensure_firewall(config.firewall)?;
            // The proxy runs in the parent (it has real network access).
            proxy_shutdown = Some(spawn_proxy(
                domains.clone(),
                v.host_ip(),
                v.jail_ip(),
                config.proxy_allowed_ports.as_deref(),
            )?);
            jail_ip = Some(v.jail_ip());

            parent_send(fd, "the veth acknowledgement")?;
        }
        let veth_pair = veth.take();

        // All setup succeeded — disarm the guard so Drop doesn't kill the child.
        child_guard.disarm();

        // `from_owned_fd` fails when tokio can't register the fd with its
        // reactor (no runtime, fd isn't a pipe, etc.). Map to `Io` — the
        // error is an `std::io::Error`, not an rustix `Errno`.
        let stdout = OutputStream::from_owned_fd(stdout_pipe.read).map_err(JailError::Io)?;
        let stderr = OutputStream::from_owned_fd(stderr_pipe.read).map_err(JailError::Io)?;

        let timeout = if config.timeout_secs > 0 {
            Duration::from_secs(config.timeout_secs)
        } else {
            Duration::from_secs(u64::MAX)
        };

        Ok(JailHandle {
            pid: child_pid,
            reaped: false,
            stdout,
            stderr,
            start_time: Instant::now(),
            timeout,
            cgroup,
            veth: veth_pair,
            jail_ip,
            proxy_shutdown,
            events: None,
            _jail_root: jail_root,
        })
    }

    /// Run a command and wait for completion.
    pub async fn run(&self, cmd: &str, args: &[&str]) -> Result<Output> {
        let handle = self.spawn(cmd, args)?;
        handle.wait().await
    }

    /// Spawn with an event stream for monitoring.
    ///
    /// The receiver gets [`JailEvent::Started`] immediately, then
    /// `Stdout`/`Stderr` lines and `Completed`/`TimedOut`/`OomKilled`
    /// while [`JailHandle::wait`] runs (the handle keeps the sender and
    /// `wait` streams through it).
    pub fn spawn_with_events(
        &self,
        cmd: &str,
        args: &[&str],
    ) -> Result<(JailHandle, EventReceiver)> {
        let mut handle = self.spawn(cmd, args)?;
        let (tx, rx) = events::channel();
        let _ = tx.send(JailEvent::Started { pid: handle.pid });
        handle.events = Some(tx);
        Ok((handle, rx))
    }

    /// Fork a running jail by cloning its filesystem state.
    ///
    /// Returns a new [`Jail`] with an identical configuration but pointing
    /// at the cloned output directory. Use the normal [`spawn`](Jail::spawn),
    /// [`run`](Jail::run), or [`spawn_with_events`](Jail::spawn_with_events)
    /// methods on the returned jail to execute commands inside the fork.
    ///
    /// With a running handle, the jail is frozen (cgroup freezer) for the
    /// duration of the clone, then thawed, so the copy is consistent. If
    /// it can't be frozen (no cgroup because the jail has no resource
    /// limits, or the freeze timed out) this fails instead of copying a
    /// tree that is still changing.
    ///
    /// On COW-capable filesystems (btrfs, xfs with reflink) the clone is
    /// nearly instant — data blocks are shared and only diverge on write.
    ///
    /// # Arguments
    ///
    /// * `running` — Handle to the running jail whose output directory will
    ///   be cloned; it is frozen during the clone. `None` copies without
    ///   freezing: use it when nothing runs in the jail, or to opt into a
    ///   hot copy of a live one (the walk never follows symlinks either way).
    /// * `fork_output` — Output directory for the forked jail. Created if
    ///   it does not exist.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (forked, info) = jail.live_fork(Some(&handle), "/tmp/fork-output")?;
    /// let result = forked.run("python", &["evaluate.py"]).await?;
    /// // or: let handle = forked.spawn("python", &["evaluate.py"])?;
    /// ```
    pub fn live_fork(
        &self,
        running: Option<&JailHandle>,
        fork_output: impl Into<PathBuf>,
    ) -> Result<(Jail, ForkInfo)> {
        let fork_output = fork_output.into();

        // Freeze the running jail for a consistent copy; refuse rather
        // than copy a tree it is still changing.
        if let Some(h) = running {
            h.freeze()?;
        }
        let frozen = running.is_some();

        // COW-clone the output directory.
        let clone_result = fork::cow_clone(&self.config.output, &fork_output);

        // Thaw immediately — even if the clone failed.
        if frozen && let Some(h) = running {
            let _ = h.thaw();
        }

        let mut fork_info = clone_result?;
        fork_info.was_frozen = frozen;

        // Build a forked Jail that shares everything except the output dir.
        // GPU resources and seccomp BPF are re-used verbatim; the only
        // diff is the output path, so we deep-clone `JailConfig` just
        // for that single field then re-Arc.
        let mut fork_config = (*self.config).clone();
        fork_config.output = fork_output;

        let fork_jail = Jail {
            config: Arc::new(fork_config),
            gpu_resources: self.gpu_resources.clone(),
            seccomp_filter: self.seccomp_filter.clone(),
        };

        Ok((fork_jail, fork_info))
    }
}

impl JailHandle {
    /// Wait for the process to complete and collect output.
    ///
    /// stdout and stderr are drained by background tasks *concurrently*
    /// with the wait on the child. Without this, a child that emits
    /// more than one pipe-buffer's worth (~64 KiB on Linux) would block
    /// on `write()` forever if reading only started after it exited.
    ///
    /// For a handle from [`Jail::spawn_with_events`] this streams events
    /// (see [`wait_with_events`](Self::wait_with_events)).
    pub async fn wait(mut self) -> Result<Output> {
        if let Some(tx) = self.events.take() {
            return self.wait_with_events(tx).await;
        }
        let pid = self.pid;
        let timeout = self.timeout;
        let start_time = self.start_time;
        let mut timed_out = false;

        // Move the streams out so the drain tasks can own them.
        // `OutputStream::closed()` is a no-op placeholder.
        let mut stdout_stream = std::mem::replace(&mut self.stdout, OutputStream::closed());
        let mut stderr_stream = std::mem::replace(&mut self.stderr, OutputStream::closed());

        let stdout_task = tokio::spawn(async move { stdout_stream.read_all().await });
        let stderr_task = tokio::spawn(async move { stderr_stream.read_all().await });

        let remaining = timeout.saturating_sub(start_time.elapsed());
        let wait_result = tokio::time::timeout(remaining, wait_for_pid(pid)).await;

        let exit_code = match wait_result {
            Ok(code) => code,
            Err(_) => {
                timed_out = true;
                kill_tree(pid);
                wait_for_pid(pid).await
            }
        };

        // Collect stats before cgroup is cleaned up
        let stats = self.collect_stats();
        let oom_killed = stats.as_ref().map(|s| s.oom_killed).unwrap_or(false);

        // Process is dead → pipes EOF → drain tasks return.
        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();

        // Mark as reaped so Drop doesn't kill a recycled PID.
        self.reaped = true;

        // Clean up veth interface (removes both ends + stops proxy bind)
        self.cleanup_veth();

        Ok(Output {
            stdout,
            stderr,
            exit_code,
            duration: start_time.elapsed(),
            timed_out,
            oom_killed,
            stats,
        })
    }

    #[must_use]
    pub fn pid(&self) -> JailPid {
        self.pid
    }

    /// Jail-side IPv4 address on the veth pair, for direct host→jail
    /// reachability. `Some` only when the jail was spawned with
    /// `Network::Allowlist`; `None` for `Network::None` and
    /// `Network::Loopback` (no veth, no routable address).
    ///
    /// Needed for the future hostname-to-jail-port forwarder: the
    /// gateway resolves a request to `http://<jail_ip>:<vm_port>/`.
    #[must_use]
    pub fn jail_ip(&self) -> Option<std::net::Ipv4Addr> {
        self.jail_ip
    }

    pub fn kill(&self) {
        kill_tree(self.pid);
    }

    /// Freeze every process in this jail via the cgroup freezer so the
    /// filesystem is quiescent for a `live_fork`. Fails when the jail has
    /// no cgroup (created without resource limits): it can't be frozen.
    pub(crate) fn freeze(&self) -> Result<()> {
        match &self.cgroup {
            Some(cg) => cg.freeze(),
            None => Err(JailError::Unsupported(
                "the jail has no cgroup (no resource limits), so it can't be frozen".into(),
            )),
        }
    }

    pub(crate) fn thaw(&self) -> Result<()> {
        if let Some(ref cg) = self.cgroup {
            cg.thaw()
        } else {
            Ok(())
        }
    }

    /// Get current resource usage (live monitoring).
    #[must_use]
    pub fn stats(&self) -> Option<ResourceStats> {
        self.collect_stats()
    }

    /// Path to the underlying cgroup directory, suitable for a detached
    /// background sampler. Returns `None` when cgroups aren't configured.
    #[must_use]
    pub fn cgroup_path(&self) -> Option<std::path::PathBuf> {
        self.cgroup.as_ref().map(|c| c.path().to_path_buf())
    }

    fn collect_stats(&self) -> Option<ResourceStats> {
        let cg = self.cgroup.as_ref()?;
        let io = cg.io_stats().unwrap_or_default();
        Some(ResourceStats {
            memory_peak_bytes: cg.memory_peak().unwrap_or(0),
            memory_current_bytes: cg.memory_current().unwrap_or(0),
            cpu_usage_usec: cg.cpu_usage_usec().unwrap_or(0),
            oom_killed: cg.oom_killed(),
            io_read_bytes: io.read_bytes,
            io_write_bytes: io.write_bytes,
            pids_current: cg.pids_current().unwrap_or(0),
        })
    }

    /// Shut down the proxy, then remove the veth pair (and release its
    /// id). Idempotent.
    fn cleanup_veth(&mut self) {
        if let Some(tx) = self.proxy_shutdown.take() {
            let _ = tx.send(true);
        }
        drop(self.veth.take());
    }

    /// Wait while streaming events to the sender.
    ///
    /// Streams stdout/stderr line by line and sends completion event.
    /// The returned `Output` buffers at most 256 MiB per stream (like
    /// [`wait`](Self::wait)); later lines are still streamed as events.
    pub async fn wait_with_events(mut self, tx: EventSender) -> Result<Output> {
        let pid = self.pid;
        let timeout = self.timeout;
        let start_time = self.start_time;
        let mut timed_out = false;

        // One drain task per stream, each owning its reader. (Selecting
        // over two `read_line` futures is not cancel-safe: a partially
        // read line is lost whenever the other branch wins.)
        fn drain(
            mut stream: OutputStream,
            tx: EventSender,
            wrap: fn(String) -> JailEvent,
        ) -> tokio::task::JoinHandle<Vec<u8>> {
            tokio::spawn(async move {
                let mut all = Vec::new();
                while let Some(line) = stream.read_line().await {
                    append_capped(&mut all, line.as_bytes(), MAX_EVENT_OUTPUT_BYTES);
                    let _ = tx.send(wrap(line.trim_end().to_string()));
                }
                all
            })
        }
        let stdout_stream = std::mem::replace(&mut self.stdout, OutputStream::closed());
        let stderr_stream = std::mem::replace(&mut self.stderr, OutputStream::closed());
        let stdout_task = drain(stdout_stream, tx.clone(), JailEvent::Stdout);
        let stderr_task = drain(stderr_stream, tx.clone(), JailEvent::Stderr);

        let remaining = timeout.saturating_sub(start_time.elapsed());
        let exit_code = match tokio::time::timeout(remaining, wait_for_pid(pid)).await {
            Ok(code) => code,
            Err(_) => {
                timed_out = true;
                let _ = tx.send(JailEvent::TimedOut);
                kill_tree(pid);
                wait_for_pid(pid).await
            }
        };

        // Process is dead → pipes EOF → drain tasks finish, so every
        // line event precedes `Completed`.
        let all_stdout = stdout_task.await.unwrap_or_default();
        let all_stderr = stderr_task.await.unwrap_or_default();

        let duration = start_time.elapsed();
        let stats = self.collect_stats();
        let oom_killed = stats.as_ref().map(|s| s.oom_killed).unwrap_or(false);

        if !timed_out {
            let _ = tx.send(JailEvent::Completed {
                exit_code,
                duration,
            });
        }

        if oom_killed {
            let _ = tx.send(JailEvent::OomKilled);
        }

        // Mark as reaped so Drop doesn't kill a recycled PID.
        self.reaped = true;

        // Clean up veth interface
        self.cleanup_veth();

        Ok(Output {
            stdout: all_stdout,
            stderr: all_stderr,
            exit_code,
            duration,
            timed_out,
            oom_killed,
            stats,
        })
    }
}

impl Drop for JailHandle {
    fn drop(&mut self) {
        if self.reaped {
            self.cleanup_veth();
            return;
        }
        kill_tree(self.pid);
        // Non-blocking spin first (avoids blocking tokio), then final
        // blocking waitpid to guarantee no zombie under memory pressure.
        if let Some(rpid) = self.pid.to_rustix() {
            for _ in 0..10 {
                match waitpid(Some(rpid), WaitOptions::NOHANG) {
                    Ok(Some(_)) | Err(_) => {
                        self.cleanup_veth();
                        return;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        }
        unsafe { libc::waitpid(self.pid.as_i32(), std::ptr::null_mut(), 0) };
        self.cleanup_veth();
    }
}

/// Give `root` and everything below it to `uid:gid`.
///
/// Walks with `openat(O_NOFOLLOW)` + `fchownat(AT_SYMLINK_NOFOLLOW)` so a
/// jail that is still running in the same tree can't redirect the walk
/// through a symlink swap. Symlinks themselves are re-owned, never
/// followed; other filesystems mounted below `root` are left alone.
/// Entries that already have the right owner are only `stat`ed.
fn chown_tree(root: &std::path::Path, uid: u32, gid: u32) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, statat};

    fn fix(
        dir: &impl std::os::fd::AsFd,
        name: &std::ffi::CStr,
        st: &rustix::fs::Stat,
        uid: u32,
        gid: u32,
    ) -> std::io::Result<()> {
        if st.st_uid == uid && st.st_gid == gid {
            return Ok(());
        }
        // SAFETY: valid dirfd and NUL-terminated name.
        let r = unsafe {
            libc::fchownat(
                dir.as_fd().as_raw_fd(),
                name.as_ptr(),
                uid,
                gid,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if r == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn walk(dir: OwnedFd, dev: u64, uid: u32, gid: u32) -> std::io::Result<()> {
        for entry in rustix::fs::Dir::read_from(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            let st = statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
            fix(&dir, name, &st, uid, gid)?;
            if (st.st_mode & libc::S_IFMT) == libc::S_IFDIR && st.st_dev == dev {
                let sub = openat(
                    &dir,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                walk(sub, dev, uid, gid)?;
            }
        }
        Ok(())
    }

    let top = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let st = rustix::fs::fstat(&top)?;
    fix(&top, c".", &st, uid, gid)?;
    walk(top, st.st_dev, uid, gid)
}

/// Ids chosen for one spawn (see [`Jail::jail_ids`]).
struct JailIds {
    /// Host uid/gid of the jail's processes and files.
    host: (u32, u32),
    /// uid/gid map target (`user_namespace = true`).
    map: Option<(u32, u32)>,
    /// Ids the jail's init switches to (`user_namespace = false`, root).
    run_as: Option<(u32, u32)>,
}

/// Raw fds the child rewires right after fork.
struct ChildFds {
    /// `/dev/null` for stdin, or `None` to inherit.
    stdin: Option<RawFd>,
    stdout: RawFd,
    stderr: RawFd,
    /// Child end of the cgroup barrier.
    barrier: RawFd,
    /// The parent's ends; closed in the child.
    parent_ends: [Option<RawFd>; 5],
}

/// What the child needs, handed to the spawner thread for the fork.
struct ForkArgs<'a> {
    ctx: &'a exec::ChildCtx<'a>,
    fds: &'a ChildFds,
    parent_pid: libc::pid_t,
}

// SAFETY: the spawner thread only moves these references into the
// forked child, which reads them in its own copy of the address space.
// The parent side never dereferences them there, and `spawner::run`
// blocks the owning thread until the fork returned.
unsafe impl Send for ForkArgs<'_> {}

/// First code the forked child runs. Never returns.
fn child_entry(ctx: &exec::ChildCtx<'_>, fds: &ChildFds, parent_pid: libc::pid_t) -> ! {
    // SAFETY (whole fn): raw syscalls on fds we own, scalar args.
    unsafe {
        // Die if the parent dies — prevents veth/cgroup leaks. If the
        // parent already died before this call, we've been reparented.
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
            exec::fail("prctl(PR_SET_PDEATHSIG)", 0);
        }
        if libc::getppid() != parent_pid {
            libc::_exit(127);
        }
        // The spawner thread blocks every signal; the jail starts from
        // an empty mask, whatever the embedder's threads block.
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
            exec::fail("sigprocmask", 0);
        }

        // New session + process group so kill_tree reaches every
        // descendant; also detaches from the controlling terminal.
        if libc::setsid() == -1 {
            exec::fail("setsid", 0);
        }

        if let Some(fd) = fds.stdin
            && libc::dup2(fd, libc::STDIN_FILENO) == -1
        {
            exec::fail("dup2(stdin)", 0);
        }
        if libc::dup2(fds.stdout, libc::STDOUT_FILENO) == -1 {
            exec::fail("dup2(stdout)", 0);
        }
        if libc::dup2(fds.stderr, libc::STDERR_FILENO) == -1 {
            exec::fail("dup2(stderr)", 0);
        }
        for fd in fds.parent_ends.iter().flatten() {
            libc::close(*fd);
        }
    }

    #[cfg(test)]
    match TEST_CHILD_MODE.load(Ordering::Relaxed) {
        TEST_CHILD_EXIT_EARLY => unsafe { libc::_exit(1) },
        TEST_CHILD_PANIC => panic!("test-induced child panic"),
        _ => {}
    }

    // Block until the parent assigned the cgroup — closes the window in
    // which the child would run without memory/CPU/PID limits.
    match exec::recv_byte(fds.barrier, exec::CHILD_SYNC_TIMEOUT) {
        exec::Recv::Ok => {}
        _ => exec::fail("cgroup barrier", 0),
    }
    // SAFETY: our fd; done with it.
    unsafe { libc::close(fds.barrier) };

    exec::run_child(ctx)
}

/// Append `data` to `buf` without letting `buf` grow past `cap` bytes.
fn append_capped(buf: &mut Vec<u8>, data: &[u8], cap: usize) {
    let room = cap.saturating_sub(buf.len());
    buf.extend_from_slice(&data[..data.len().min(room)]);
}

/// RAII guard that kills + reaps a child on drop (error paths after fork).
struct ChildGuard(JailPid);

impl ChildGuard {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        kill_tree(self.0);
        unsafe { libc::waitpid(self.0.as_i32(), std::ptr::null_mut(), 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Serialises tests that flip the global child-mode hook.
    static HOOK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn dirs(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("aj-unit-{name}-{}", std::process::id()));
        let (src, out) = (base.join("src"), base.join("out"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        (src, out)
    }

    fn privileged() -> bool {
        rustix::process::geteuid().is_root()
    }

    #[test]
    fn append_capped_never_exceeds_cap() {
        let mut buf = Vec::new();
        append_capped(&mut buf, b"hello ", 8);
        append_capped(&mut buf, b"world", 8);
        append_capped(&mut buf, b"!!", 8);
        assert_eq!(buf, b"hello wo");
    }

    /// Item 3: a child that dies before the handshake must make `spawn`
    /// fail fast instead of blocking forever on a socket the parent
    /// itself kept open.
    #[tokio::test]
    async fn spawn_fails_fast_when_child_dies_before_sync() {
        if !privileged() {
            eprintln!("skip: needs root");
            return;
        }
        let _g = HOOK_LOCK.lock().await;
        for (name, network) in [
            ("userns", Network::None),
            ("veth", Network::Allowlist(vec!["example.invalid".into()])),
        ] {
            let (src, out) = dirs(&format!("early-exit-{name}"));
            let config = JailConfig {
                source: src,
                output: out,
                network,
                landlock: false,
                // No cgroup: exercise the userns/netns handshake, not
                // `cgroup.procs` rejecting a dead pid.
                memory_mb: None,
                cpu_percent: 0,
                max_pids: 0,
                ..Default::default()
            };
            let jail = Jail::new(config).unwrap();
            TEST_CHILD_MODE.store(TEST_CHILD_EXIT_EARLY, Ordering::SeqCst);
            let started = Instant::now();
            // Plain thread (not spawn_blocking) so a hang fails the test
            // instead of blocking runtime shutdown.
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(jail.spawn("/bin/true", &[]).map(|_| ()));
            });
            let r = tokio::time::timeout(Duration::from_secs(10), rx).await;
            TEST_CHILD_MODE.store(0, Ordering::SeqCst);
            let r = r.expect("spawn hung after the child died").unwrap();
            assert!(
                matches!(r, Err(JailError::Handshake(_))),
                "{name}: expected a handshake error, got {r:?}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{name}: took {:?}",
                started.elapsed()
            );
        }
    }

    /// Item 6: a panic in the forked child must stay in the child
    /// (exit 127), never unwind into the duplicated embedder.
    #[tokio::test]
    async fn child_panic_is_contained() {
        if !privileged() {
            eprintln!("skip: needs root");
            return;
        }
        let _g = HOOK_LOCK.lock().await;
        let (src, out) = dirs("child-panic");
        let config = JailConfig {
            source: src,
            output: out,
            user_namespace: false,
            landlock: false,
            memory_mb: None,
            cpu_percent: 0,
            max_pids: 0,
            ..Default::default()
        };
        let jail = Jail::new(config).unwrap();
        TEST_CHILD_MODE.store(TEST_CHILD_PANIC, Ordering::SeqCst);
        let r = jail.spawn("/bin/true", &[]);
        TEST_CHILD_MODE.store(0, Ordering::SeqCst);
        match r {
            // The child panicked before the barrier: the parent sees
            // either a closed barrier socket or a quick exit.
            Err(JailError::Handshake(_)) => {}
            Ok(h) => {
                let o = h.wait().await.unwrap();
                assert_eq!(o.exit_code, 127);
                assert!(String::from_utf8_lossy(&o.stderr).contains("panicked"));
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
