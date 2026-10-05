//! Jail execution: fork, setup, exec.

use crate::cgroup::Cgroup;
use crate::config::{JailConfig, Network};
use crate::error::{JailError, Result};
use crate::events::{EventReceiver, EventSender, JailEvent};
use crate::fork::{self, ForkInfo};
use crate::namespace::write_uid_gid_map;
use crate::pipe::{OutputStream, Pipe};
use crate::run_internal::{extract_exit_code, kill_tree, wait_for_pid};
use crate::veth::{NEXT_VETH_ID, spawn_allowlist_proxy, sync_socketpair, veth_addrs};
use crate::{events, exec, gpu, netlink};

use rustix::process::{Pid, WaitOptions, waitpid};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
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
/// the underlying `Vec<String>` allowlist or the env table. Matters
/// for our high-throughput target (tens of thousands of jails/sec).
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
    /// Host-side veth interface name to clean up (Allowlist mode only).
    veth_host_iface: Option<String>,
    /// Jail-side IPv4 address on the veth pair (`Some` only when the
    /// jail was spawned with `Network::Allowlist`). The host can reach
    /// this address directly for inbound forwarding.
    jail_ip: Option<std::net::Ipv4Addr>,
    /// Shutdown signal for the proxy thread (Allowlist mode only).
    proxy_shutdown: Option<tokio::sync::watch::Sender<bool>>,
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

impl Jail {
    /// Create a new jail from configuration.
    ///
    /// Validates paths and discovers GPU resources upfront so errors
    /// are reported before forking.
    pub fn new(config: JailConfig) -> Result<Self> {
        if !config.source.exists() {
            return Err(JailError::PathNotFound(config.source.clone()));
        }
        if !config.output.exists() {
            return Err(JailError::PathNotFound(config.output.clone()));
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
        let has_limits = config.memory_mb > 0
            || config.cpu_percent > 0
            || config.max_pids > 0
            || config.io_read_mbps > 0
            || config.io_write_mbps > 0;

        if !has_limits {
            return Ok(None);
        }

        let name = format!("{}-{}", std::process::id(), pid.as_raw());
        let cg = Cgroup::create(&name)?;

        if config.memory_mb > 0 {
            cg.set_memory_limit(config.memory_mb * 1024 * 1024)?;
        }
        if config.cpu_percent > 0 {
            cg.set_cpu_quota(config.cpu_percent)?;
        }
        if config.max_pids > 0 {
            cg.set_pids_max(config.max_pids)?;
        }
        if config.io_read_mbps > 0 || config.io_write_mbps > 0 {
            let read_bps = config.io_read_mbps * 1024 * 1024;
            let write_bps = config.io_write_mbps * 1024 * 1024;
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

    /// Spawn a command in the jail.
    pub fn spawn(&self, cmd: &str, args: &[&str]) -> Result<JailHandle> {
        let stdout_pipe = Pipe::new()?;
        let stderr_pipe = Pipe::new()?;

        // Barrier pipe: child blocks until parent has assigned the cgroup.
        // Without this, the child runs unconstrained (no memory/CPU/PID limits)
        // during the entire parent-side setup phase.
        let barrier_pipe = Pipe::new()?;

        // For Allowlist mode, we need a sync channel so the child can signal
        // "I've entered my network namespace" and the parent can reply with
        // the veth ID after setting up the network bridge.
        let needs_veth = matches!(self.config.network, Network::Allowlist(_));
        let sync_pair = if needs_veth {
            Some(sync_socketpair()?)
        } else {
            None
        };

        // For user_namespace mode, a second socketpair coordinates the
        // userns handoff: child unshares NEWUSER, signals us, we write
        // uid_map/gid_map against the *new* namespace (not the parent's
        // init userns), then we signal back so the child can finish
        // unsharing the remaining namespaces.
        let userns_pair = if self.config.user_namespace {
            Some(sync_socketpair()?)
        } else {
            None
        };

        // All three clones are `Arc::clone` — refcount bumps, no deep
        // copy of config, env, allowlist, or seccomp BPF.
        let config = self.config.clone();
        let gpu_resources = self.gpu_resources.clone();
        let seccomp_filter = self.seccomp_filter.clone();
        let cmd = cmd.to_string();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();

        // Extract child-side fd before fork (child gets fds[0])
        let child_sync_fd = sync_pair.as_ref().map(|(child_fd, _)| child_fd.as_raw_fd());
        let child_userns_fd = userns_pair.as_ref().map(|(child_fd, _)| child_fd.as_raw_fd());
        let barrier_read_fd = barrier_pipe.read.as_raw_fd();

        // SAFETY: fork() is safe when we immediately either _exit() or exec() in child.
        // Parent continues normally after fork returns.
        let child_pid = unsafe {
            match libc::fork() {
                -1 => {
                    return Err(JailError::Fork(rustix::io::Errno::from_raw_os_error(
                        std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                    )));
                }
                0 => {
                    // Child process
                    // Die if parent is killed — prevents veth interface leaks.
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);

                    // Create new session and process group so we can kill all descendants.
                    if libc::setsid() == -1 {
                        libc::_exit(127);
                    }

                    libc::dup2(stdout_pipe.write.as_raw_fd(), libc::STDOUT_FILENO);
                    libc::dup2(stderr_pipe.write.as_raw_fd(), libc::STDERR_FILENO);
                    drop(stdout_pipe);
                    drop(stderr_pipe);

                    // Block until parent signals that the cgroup is assigned.
                    // This closes the resource-limit bypass window.
                    let mut go = [0u8; 1];
                    let _ = libc::read(barrier_read_fd, go.as_mut_ptr() as *mut _, 1);
                    drop(barrier_pipe);

                    if let Err(e) = exec::setup_child(
                        &config,
                        gpu_resources.as_deref(),
                        seccomp_filter.as_deref(),
                        &cmd,
                        &args,
                        child_sync_fd,
                        child_userns_fd,
                    ) {
                        eprintln!("jail setup failed: {e}");
                        libc::_exit(127);
                    }
                    unreachable!()
                }
                pid => JailPid(pid as u32),
            }
        };

        // Parent: close write ends of stdout/stderr, read end of barrier
        drop(stdout_pipe.write);
        drop(stderr_pipe.write);
        drop(barrier_pipe.read);

        // Guard: if anything below fails, kill and reap the child so it
        // doesn't become a zombie leaking PIDs, cgroups and veth interfaces.
        let child_guard = ChildGuard(child_pid);

        // Write UID/GID maps if using user namespace. Ordering:
        //   1. Child unshares NEWUSER, writes 1 byte on userns_pair.
        //   2. We read that byte, proving the child is in its own
        //      user namespace — so `/proc/<pid>/uid_map` now refers
        //      to *that* ns, not our init namespace.
        //   3. We write setgroups/uid_map/gid_map.
        //   4. We write 1 byte back; the child wakes and proceeds.
        if let (true, Some((_child_fd, parent_fd))) = (config.user_namespace, &userns_pair) {
            let mut ready = [0u8; 1];
            // SAFETY: valid fd from socketpair, one-byte read.
            let n = unsafe {
                libc::read(parent_fd.as_raw_fd(), ready.as_mut_ptr() as *mut _, 1)
            };
            if n != 1 {
                return Err(JailError::UidMap(std::io::Error::other(
                    "userns ready sync failed",
                )));
            }

            if let Some(pid) = child_pid.to_rustix()
                && let Err(e) = write_uid_gid_map(pid)
            {
                if rustix::process::getuid().is_root() {
                    // Running as real root the map write is redundant
                    // and some kernels refuse it; don't fail the spawn.
                    eprintln!("warning: uid/gid map failed (running as root): {e}");
                } else {
                    return Err(e);
                }
            }

            // Release child from the userns barrier.
            // SAFETY: valid fd from socketpair, one-byte write.
            let n = unsafe {
                libc::write(parent_fd.as_raw_fd(), [1u8].as_ptr() as *const _, 1)
            };
            if n != 1 {
                return Err(JailError::UidMap(std::io::Error::other(
                    "userns ack sync failed",
                )));
            }
        }

        // Create and configure cgroup BEFORE allowing child to proceed.
        let cgroup = self.create_cgroup(child_pid)?;
        if let Some(ref cg) = cgroup {
            cg.add_pid(child_pid.as_raw())?;
        }

        // Signal child: cgroup is assigned, proceed with setup_child.
        // SAFETY: Valid fd from pipe, writing 1 byte.
        unsafe { libc::write(barrier_pipe.write.as_raw_fd(), [1u8].as_ptr() as *const _, 1) };
        drop(barrier_pipe.write);

        // For Allowlist mode: wait for child to enter netns, then set up veth + proxy
        let mut proxy_shutdown = None;
        let mut veth_iface_name = None;
        let mut jail_ip: Option<std::net::Ipv4Addr> = None;
        if let (Some((_child_fd, parent_fd)), Network::Allowlist(domains)) =
            (sync_pair, &config.network)
        {
            // Wait for child to signal "I'm in my network namespace"
            let mut buf = [0u8; 1];
            // SAFETY: Valid fd from socketpair, reading 1 byte.
            let n = unsafe { libc::read(parent_fd.as_raw_fd(), buf.as_mut_ptr() as *mut _, 1) };
            if n != 1 {
                return Err(JailError::Network(std::io::Error::other("child netns sync failed")));
            }

            let id = NEXT_VETH_ID.fetch_add(1, Ordering::Relaxed);
            let (host_ip, this_jail_ip) = veth_addrs(id);
            jail_ip = Some(this_jail_ip);
            let host_if = format!("aj-h{id}");
            let jail_if = format!("aj-j{id}");

            // Create veth pair, move jail end into child netns, configure host end
            netlink::create_veth_pair(&host_if, &jail_if)?;
            netlink::move_to_netns(&jail_if, child_pid.as_raw())?;
            netlink::add_ipv4_addr(&host_if, host_ip, 30)?;
            netlink::set_link_up(&host_if)?;

            // Spawn proxy in parent (has real network access)
            proxy_shutdown = Some(spawn_allowlist_proxy(domains.clone(), host_ip));

            // Signal child with the veth ID so it can derive IPs
            let id_bytes = id.to_le_bytes();
            // SAFETY: Valid fd from socketpair, writing 4 bytes.
            let n = unsafe { libc::write(parent_fd.as_raw_fd(), id_bytes.as_ptr() as *const _, 4) };
            if n != 4 {
                return Err(JailError::Network(std::io::Error::other("veth ID sync failed")));
            }

            veth_iface_name = Some(host_if);
        }

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
            veth_host_iface: veth_iface_name,
            jail_ip,
            proxy_shutdown,
        })
    }

    /// Run a command and wait for completion.
    pub async fn run(&self, cmd: &str, args: &[&str]) -> Result<Output> {
        let handle = self.spawn(cmd, args)?;
        handle.wait().await
    }

    /// Spawn with event stream for monitoring.
    pub fn spawn_with_events(
        &self,
        cmd: &str,
        args: &[&str],
    ) -> Result<(JailHandle, EventReceiver)> {
        let handle = self.spawn(cmd, args)?;
        let (tx, rx) = events::channel();

        // Send started event
        let _ = tx.send(JailEvent::Started { pid: handle.pid });

        Ok((handle, rx))
    }

    /// Fork a running jail by cloning its filesystem state.
    ///
    /// Returns a new [`Jail`] with an identical configuration but pointing
    /// at the cloned output directory. Use the normal [`spawn`](Jail::spawn),
    /// [`run`](Jail::run), or [`spawn_with_events`](Jail::spawn_with_events)
    /// methods on the returned jail to execute commands inside the fork.
    ///
    /// The original jail continues running uninterrupted. If a running
    /// handle is provided, the jail is frozen for sub-millisecond during
    /// the clone for a consistent snapshot, then immediately thawed.
    ///
    /// On COW-capable filesystems (btrfs, xfs with reflink) the clone is
    /// nearly instant — data blocks are shared and only diverge on write.
    ///
    /// # Arguments
    ///
    /// * `running` — Handle to the running jail whose output directory will
    ///   be cloned. If `Some`, the jail's cgroup is frozen for a consistent
    ///   snapshot. Pass `None` to skip freezing.
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

        // Freeze the running jail for a consistent snapshot.
        let frozen = running
            .map(|h| h.freeze().is_ok())
            .unwrap_or(false);

        // COW-clone the output directory.
        let clone_result = fork::cow_clone(&self.config.output, &fork_output);

        // Thaw immediately — even if the clone failed.
        if frozen
            && let Some(h) = running {
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
    /// on `write()` forever — the old code only started reading after
    /// the child exited.
    pub async fn wait(mut self) -> Result<Output> {
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
    /// filesystem is quiescent for a `live_fork`. Sub-millisecond; no-op
    /// when the jail was created without cgroup limits.
    pub(crate) fn freeze(&self) -> Result<()> {
        if let Some(ref cg) = self.cgroup {
            cg.freeze()
        } else {
            Ok(())
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
            memory_peak_bytes:    cg.memory_peak().unwrap_or(0),
            memory_current_bytes: cg.memory_current().unwrap_or(0),
            cpu_usage_usec:       cg.cpu_usage_usec().unwrap_or(0),
            oom_killed:           cg.oom_killed(),
            io_read_bytes:        io.read_bytes,
            io_write_bytes:       io.write_bytes,
            pids_current:         cg.pids_current().unwrap_or(0),
        })
    }

    /// Shut down the proxy and remove the host-side veth interface.
    fn cleanup_veth(&mut self) {
        // Signal proxy to stop
        if let Some(tx) = self.proxy_shutdown.take() {
            let _ = tx.send(true);
        }
        if let Some(iface) = self.veth_host_iface.take() {
            let _ = netlink::delete_link(&iface);
        }
    }

    /// Wait while streaming events to the sender.
    ///
    /// Streams stdout/stderr line by line and sends completion event.
    pub async fn wait_with_events(mut self, tx: EventSender) -> Result<Output> {
        let pid = self.pid;
        let timeout = self.timeout;
        let start_time = self.start_time;

        let mut all_stdout = Vec::new();
        let mut all_stderr = Vec::new();
        let mut timed_out = false;
        let mut stdout_done = false;
        let mut stderr_done = false;

        let remaining = timeout.saturating_sub(start_time.elapsed());

        let result = tokio::time::timeout(remaining, async {
            loop {
                tokio::select! {
                    line = self.stdout.read_line(), if !stdout_done => {
                        match line {
                            Some(l) => {
                                all_stdout.extend_from_slice(l.as_bytes());
                                let _ = tx.send(JailEvent::Stdout(l.trim_end().to_string()));
                            }
                            None => stdout_done = true,
                        }
                    }
                    line = self.stderr.read_line(), if !stderr_done => {
                        match line {
                            Some(l) => {
                                all_stderr.extend_from_slice(l.as_bytes());
                                let _ = tx.send(JailEvent::Stderr(l.trim_end().to_string()));
                            }
                            None => stderr_done = true,
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)), if stdout_done && stderr_done => {
                        // Both streams closed, check if process exited
                        if let Ok(Some(status)) = waitpid(pid.to_rustix(), WaitOptions::NOHANG) {
                            return extract_exit_code(status);
                        }
                    }
                }
            }
        })
        .await;

        let exit_code = match result {
            Ok(code) => code,
            Err(_) => {
                timed_out = true;
                let _ = tx.send(JailEvent::TimedOut);
                kill_tree(pid);
                wait_for_pid(pid).await
            }
        };

        let duration = start_time.elapsed();
        let stats = self.collect_stats();
        let oom_killed = stats.as_ref().map(|s| s.oom_killed).unwrap_or(false);

        if !timed_out {
            let _ = tx.send(JailEvent::Completed { exit_code, duration });
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

/// RAII guard that kills + reaps a child on drop (error paths after fork).
struct ChildGuard(JailPid);

impl ChildGuard {
    fn disarm(self) { std::mem::forget(self); }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        kill_tree(self.0);
        unsafe { libc::waitpid(self.0.as_i32(), std::ptr::null_mut(), 0) };
    }
}
