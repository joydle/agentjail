//! Cgroups v2 resource limits.

use crate::error::{JailError, Result};
use std::fs;
use std::path::PathBuf;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Deadline for `Cgroup::freeze`. The kernel typically transitions to
/// `frozen 1` in hundreds of microseconds; we give it 50 ms before
/// giving up, because even a disk-bound task usually unblocks well
/// within that window. If a cgroup legitimately can't freeze in 50 ms
/// something deeper is wrong — better to surface than to silently
/// snapshot a running filesystem.
const FREEZE_DEADLINE_MS: u64 = 50;

/// Poll `cgroup.events` until it reports `frozen 1`. Returns the
/// observed latency in the success case and an error otherwise.
///
/// `cgroup.events` is a sysfs file that supports `poll(POLLPRI)` —
/// inotify-style notifications — but a tight `read` + sleep is simpler
/// and well-bounded at 50 ms. One outstanding freeze per jail, and we
/// only pay this cost during `live_fork`, so polling is fine.
fn wait_for_frozen(path: &std::path::Path, deadline: std::time::Duration) -> Result<()> {
    use std::time::Instant;

    let events_path = path.join("cgroup.events");
    let start = Instant::now();
    let mut backoff_us = 100u64;

    loop {
        if let Ok(contents) = fs::read_to_string(&events_path) {
            for line in contents.lines() {
                if line == "frozen 1" {
                    return Ok(());
                }
            }
        }
        if start.elapsed() >= deadline {
            return Err(JailError::Cgroup(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "cgroup did not reach frozen state within {:?}",
                    deadline
                ),
            )));
        }
        std::thread::sleep(std::time::Duration::from_micros(backoff_us));
        // Back off: 100us → 200us → 400us → 800us → 1.6ms then cap.
        // Fast to wake for the common sub-ms case, gentle for the rare
        // multi-ms case.
        backoff_us = (backoff_us * 2).min(1_600);
    }
}

/// Handle to a cgroup for resource limiting.
pub struct Cgroup {
    path: PathBuf,
}

impl Cgroup {
    /// Path to this cgroup directory — callers can read the file-based
    /// metrics directly (e.g. from a detached sampler task).
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Create a new cgroup with the given name.
    ///
    /// The cgroup is created under the current user's cgroup (for rootless)
    /// or under the system cgroup root.
    pub fn create(name: &str) -> Result<Self> {
        let base = ensure_cgroup_base()?;

        let path = base.join(format!("agentjail-{name}"));
        if !path.exists() {
            fs::create_dir(&path).map_err(JailError::Cgroup)?;
        }

        Ok(Self { path })
    }

    /// Set memory limit in bytes.
    pub fn set_memory_limit(&self, bytes: u64) -> Result<()> {
        fs::write(self.path.join("memory.max"), bytes.to_string()).map_err(JailError::Cgroup)
    }

    /// Set CPU quota as a percentage (100 = one full core).
    pub fn set_cpu_quota(&self, percent: u64) -> Result<()> {
        // cpu.max format: "quota period"
        // quota in microseconds, period typically 100000 (100ms)
        let period: u64 = 100_000;
        let quota = (period * percent) / 100;
        let value = format!("{quota} {period}");

        fs::write(self.path.join("cpu.max"), value).map_err(JailError::Cgroup)
    }

    /// Set maximum number of processes/threads.
    pub fn set_pids_max(&self, max: u64) -> Result<()> {
        fs::write(self.path.join("pids.max"), max.to_string()).map_err(JailError::Cgroup)
    }

    /// Add a process to this cgroup.
    pub fn add_pid(&self, pid: u32) -> Result<()> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string()).map_err(JailError::Cgroup)
    }

    /// Read peak memory usage in bytes.
    pub fn memory_peak(&self) -> Option<u64> {
        fs::read_to_string(self.path.join("memory.peak"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Read current memory usage in bytes at call time.
    pub fn memory_current(&self) -> Option<u64> {
        fs::read_to_string(self.path.join("memory.current"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Read the number of processes/threads currently alive in the jail.
    pub fn pids_current(&self) -> Option<u64> {
        fs::read_to_string(self.path.join("pids.current"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Read CPU usage in microseconds.
    pub fn cpu_usage_usec(&self) -> Option<u64> {
        let stat = fs::read_to_string(self.path.join("cpu.stat")).ok()?;
        for line in stat.lines() {
            if let Some(val) = line.strip_prefix("usage_usec ") {
                return val.trim().parse().ok();
            }
        }
        None
    }

    /// Check if OOM killer was triggered.
    pub fn oom_killed(&self) -> bool {
        let events = match fs::read_to_string(self.path.join("memory.events")) {
            Ok(e) => e,
            Err(_) => return false,
        };

        for line in events.lines() {
            if let Some(val) = line.strip_prefix("oom_kill ")
                && let Ok(count) = val.trim().parse::<u64>() {
                    return count > 0;
                }
        }
        false
    }

    /// Freeze all processes in this cgroup and **wait for quiescence**.
    ///
    /// Writing `1` to `cgroup.freeze` only *requests* freeze — tasks in
    /// uninterruptible syscalls (disk I/O, network) don't stop until
    /// those syscalls return. Without waiting, a follow-up snapshot
    /// reads a torn filesystem.
    ///
    /// We therefore poll `cgroup.events` (a small virtual file with
    /// `frozen 0|1` lines) until the kernel reports `frozen 1` or the
    /// deadline elapses. Typical freeze latency: **sub-millisecond**.
    /// Under heavy disk I/O the deadline gives us a bounded fallback —
    /// we return an error rather than proceed with an inconsistent
    /// snapshot.
    pub fn freeze(&self) -> Result<()> {
        fs::write(self.path.join("cgroup.freeze"), "1").map_err(JailError::Cgroup)?;
        wait_for_frozen(&self.path, std::time::Duration::from_millis(FREEZE_DEADLINE_MS))
    }

    /// Thaw (resume) all processes in this cgroup.
    ///
    /// Does not wait — thawing is already fire-and-forget from the
    /// caller's perspective; processes resume as soon as the kernel
    /// flips the state.
    pub fn thaw(&self) -> Result<()> {
        fs::write(self.path.join("cgroup.freeze"), "0").map_err(JailError::Cgroup)
    }

    /// Set I/O bandwidth limits (bytes per second).
    pub fn set_io_limit(&self, device: &str, read_bps: u64, write_bps: u64) -> Result<()> {
        let dev_id = get_device_id(device)?;
        let value = format!("{dev_id} rbps={read_bps} wbps={write_bps}");
        fs::write(self.path.join("io.max"), value).map_err(JailError::Cgroup)
    }

    /// Read I/O statistics.
    pub fn io_stats(&self) -> Option<IoStats> {
        let stat = fs::read_to_string(self.path.join("io.stat")).ok()?;
        let mut total = IoStats::default();

        for line in stat.lines() {
            // Format: "MAJ:MIN rbytes=N wbytes=N rios=N wios=N"
            for part in line.split_whitespace().skip(1) {
                if let Some(val) = part.strip_prefix("rbytes=") {
                    total.read_bytes += val.parse::<u64>().unwrap_or(0);
                } else if let Some(val) = part.strip_prefix("wbytes=") {
                    total.write_bytes += val.parse::<u64>().unwrap_or(0);
                } else if let Some(val) = part.strip_prefix("rios=") {
                    total.read_ios += val.parse::<u64>().unwrap_or(0);
                } else if let Some(val) = part.strip_prefix("wios=") {
                    total.write_ios += val.parse::<u64>().unwrap_or(0);
                }
            }
        }

        Some(total)
    }
}

/// I/O statistics from cgroup.
#[derive(Debug, Clone, Default)]
pub struct IoStats {
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub read_ios: u64,
    pub write_ios: u64,
}

/// Get device major:minor from path.
fn get_device_id(path: &str) -> Result<String> {
    use std::os::unix::fs::MetadataExt;

    let meta = fs::metadata(path).map_err(JailError::Cgroup)?;
    let dev = meta.dev();
    let major = libc::major(dev);
    let minor = libc::minor(dev);

    Ok(format!("{major}:{minor}"))
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        // Kill any remaining processes so the cgroup can be removed.
        if let Ok(procs) = fs::read_to_string(self.path.join("cgroup.procs")) {
            for line in procs.lines() {
                if let Ok(pid) = line.trim().parse::<i32>()
                    && pid > 0 {
                        unsafe { libc::kill(pid, libc::SIGKILL) };
                    }
            }
        }
        // Brief spin — processes exit almost immediately after SIGKILL.
        for _ in 0..20 {
            if fs::remove_dir(&self.path).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Last resort: log and accept the leak.
        eprintln!(
            "warning: could not remove cgroup {}, processes may still be running",
            self.path.display()
        );
    }
}

/// Find the cgroup base and ensure controllers are enabled.
///
/// Cgroup v2 "no internal process" rule: controllers can only be enabled
/// on a cgroup that has no processes. On first call, we migrate all
/// processes into `agentjail-init` and enable controllers. Subsequent
/// calls return the cached base path.
fn ensure_cgroup_base() -> Result<PathBuf> {
    use std::sync::OnceLock;
    static BASE: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();

    let result = BASE.get_or_init(|| init_cgroup_base().map_err(|e| e.to_string()));
    match result {
        Ok(p) => Ok(p.clone()),
        Err(e) => Err(JailError::Cgroup(std::io::Error::other(e.clone()))),
    }
}

fn init_cgroup_base() -> Result<PathBuf> {
    let info = fs::read_to_string("/proc/self/cgroup").map_err(JailError::Cgroup)?;
    let our_path = info.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or("/");

    let base = if our_path == "/" || our_path == "/agentjail-init" {
        PathBuf::from(CGROUP_ROOT)
    } else {
        PathBuf::from(CGROUP_ROOT).join(our_path.trim_start_matches('/'))
    };

    // Enable controllers. The kernel accepts a single space-separated
    // write — collapse the four writes into one syscall. If the first
    // write is rejected (no-internal-processes rule), drain root then
    // retry.
    let subtree_ctl = base.join("cgroup.subtree_control");
    let enable_all = "+memory +cpu +pids +io";
    if fs::write(&subtree_ctl, enable_all).is_err() {
        let init_cg = base.join("agentjail-init");
        let _ = fs::create_dir(&init_cg);
        // Move ALL processes out of root. Retry for stragglers.
        for _ in 0..20 {
            if let Ok(procs) = fs::read_to_string(base.join("cgroup.procs")) {
                for pid in procs.lines().filter(|l| !l.is_empty()) {
                    let _ = fs::write(init_cg.join("cgroup.procs"), pid);
                }
            }
            if fs::write(&subtree_ctl, enable_all).is_ok() { break; }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    Ok(base)
}
