//! Cgroups v2 resource limits.

use crate::error::{JailError, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Deadline for `Cgroup::freeze`. The kernel typically transitions to
/// `frozen 1` in hundreds of microseconds; we give it 50 ms before
/// giving up, because even a disk-bound task usually unblocks well
/// within that window. If a cgroup legitimately can't freeze in 50 ms
/// something deeper is wrong — better to surface than to silently
/// snapshot a running filesystem.
pub(crate) const FREEZE_DEADLINE: Duration = Duration::from_millis(50);

/// Poll `cgroup.events` until it reports `frozen 1`. Returns an error
/// once `deadline` elapses.
///
/// `cgroup.events` is a sysfs file that supports `poll(POLLPRI)` —
/// inotify-style notifications — but a tight `read` + sleep is simpler
/// and well-bounded at 50 ms. Back-off starts at 100 µs and caps at
/// 1.6 ms, so the common sub-ms case wakes fast and the rare multi-ms
/// case doesn't spin-loop.
fn wait_for_frozen(path: &Path, deadline: Duration) -> Result<()> {
    use std::time::Instant;

    let events_path = path.join("cgroup.events");
    let start = Instant::now();
    let mut backoff_us = 100u64;

    loop {
        if let Ok(contents) = fs::read_to_string(&events_path)
            && contents.lines().any(|l| l == "frozen 1")
        {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(JailError::Cgroup(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("cgroup did not reach frozen state within {deadline:?}"),
            )));
        }
        std::thread::sleep(Duration::from_micros(backoff_us));
        backoff_us = (backoff_us * 2).min(1_600);
    }
}

/// Freeze the cgroup at `path` and wait until the kernel reports it
/// quiescent.
///
/// **`Err` always means "not frozen".** If the freeze request is
/// accepted but the cgroup doesn't reach `frozen 1` before the deadline,
/// we write `cgroup.freeze = 0` before returning the error, so a timeout
/// never leaves the jail stuck half-frozen while the caller (who saw an
/// `Err`) skips the thaw.
pub(crate) fn freeze_path(path: &Path, deadline: Duration) -> Result<()> {
    fs::write(path.join("cgroup.freeze"), "1").map_err(JailError::Cgroup)?;
    match wait_for_frozen(path, deadline) {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Err(thaw_err) = thaw_path(path) {
                eprintln!(
                    "ERROR: freeze of {} timed out and the rollback thaw failed \
                     ({thaw_err}) — processes may remain frozen",
                    path.display()
                );
            }
            Err(e)
        }
    }
}

/// Thaw the cgroup at `path`. Idempotent and non-blocking.
pub(crate) fn thaw_path(path: &Path) -> Result<()> {
    fs::write(path.join("cgroup.freeze"), "0").map_err(JailError::Cgroup)
}

/// Handle to a cgroup for resource limiting.
pub struct Cgroup {
    path: PathBuf,
}

impl Cgroup {
    /// Path to this cgroup directory — callers can read the file-based
    /// metrics directly (e.g. from a detached sampler task).
    #[must_use]
    pub fn path(&self) -> &Path {
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

    /// Set memory limit in bytes, and disable swap for the cgroup.
    ///
    /// `memory.max` alone only caps *resident* memory: a jail can blow
    /// straight past it by getting paged out to swap, which is host disk
    /// outside the jail's accounting. Whenever a memory limit is set we
    /// therefore also write `memory.swap.max = 0`. The file only exists
    /// when the kernel does swap accounting; if it's absent there is no
    /// per-cgroup swap knob to set, so that case is not an error.
    pub fn set_memory_limit(&self, bytes: u64) -> Result<()> {
        fs::write(self.path.join("memory.max"), bytes.to_string()).map_err(JailError::Cgroup)?;
        match fs::write(self.path.join("memory.swap.max"), "0") {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(JailError::Cgroup(e)),
        }
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
                && let Ok(count) = val.trim().parse::<u64>()
            {
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
    ///
    /// On timeout the freeze is rolled back (`cgroup.freeze = 0`) before
    /// the error is returned: an `Err` always means "not frozen".
    pub fn freeze(&self) -> Result<()> {
        freeze_path(&self.path, FREEZE_DEADLINE)
    }

    /// Thaw (resume) all processes in this cgroup.
    ///
    /// Does not wait — thawing is already fire-and-forget from the
    /// caller's perspective; processes resume as soon as the kernel
    /// flips the state.
    pub fn thaw(&self) -> Result<()> {
        thaw_path(&self.path)
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
                    && pid > 0
                {
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
/// in `cgroup.subtree_control` of a cgroup that has no processes of its
/// own. We follow the systemd delegation convention: the agentjail
/// process moves **only itself** into a leaf `agentjail-init` child of
/// its cgroup, then enables controllers on the (now empty) parent, which
/// becomes the base under which every jail cgroup is created.
///
/// Other processes that share our cgroup (a supervising shell, `cargo`,
/// sidecars) are never touched. If they keep the base non-empty we
/// return an error explaining how to fix the deployment (run agentjail
/// alone in a delegated cgroup) rather than silently relocating
/// processes we don't own.
///
/// Idempotent: a process already sitting in `<base>/agentjail-init`
/// resolves to `<base>` again instead of nesting one level deeper on
/// every start. The result is cached for the life of the process.
fn ensure_cgroup_base() -> Result<PathBuf> {
    use std::sync::OnceLock;
    static BASE: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();

    let result = BASE.get_or_init(|| init_cgroup_base().map_err(|e| e.to_string()));
    match result {
        Ok(p) => Ok(p.clone()),
        Err(e) => Err(JailError::Cgroup(std::io::Error::other(e.clone()))),
    }
}

/// Name of the leaf cgroup the agentjail process parks itself in.
const INIT_LEAF: &str = "agentjail-init";

/// Controllers jails rely on. Only the ones the kernel actually offers
/// in `cgroup.controllers` are requested, so a missing `io` controller
/// doesn't make the whole enable fail.
const WANTED_CONTROLLERS: [&str; 4] = ["memory", "cpu", "pids", "io"];

fn init_cgroup_base() -> Result<PathBuf> {
    let info = fs::read_to_string("/proc/self/cgroup").map_err(JailError::Cgroup)?;
    let our_path = info
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or("/");
    init_cgroup_base_at(Path::new(CGROUP_ROOT), our_path, std::process::id())
}

/// Core of [`init_cgroup_base`], parameterised for tests.
///
/// - `root`: cgroup2 mount point (normally `/sys/fs/cgroup`).
/// - `our_path`: the process's cgroup as printed in `/proc/self/cgroup`
///   (`0::<path>`).
/// - `pid`: the process to park in the leaf. Writing a PID to
///   `cgroup.procs` migrates that whole thread group and nothing else.
pub(crate) fn init_cgroup_base_at(root: &Path, our_path: &str, pid: u32) -> Result<PathBuf> {
    let rel = our_path
        .trim()
        .trim_start_matches('/')
        .trim_end_matches('/');
    // Idempotency: already parked in `<base>/agentjail-init` → `<base>`.
    let rel = if rel == INIT_LEAF {
        ""
    } else {
        rel.strip_suffix(&format!("/{INIT_LEAF}")).unwrap_or(rel)
    };
    let base = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };

    let subtree_ctl = base.join("cgroup.subtree_control");
    let available =
        fs::read_to_string(base.join("cgroup.controllers")).map_err(JailError::Cgroup)?;
    let wanted: Vec<&str> = WANTED_CONTROLLERS
        .iter()
        .copied()
        .filter(|c| available.split_whitespace().any(|a| a == *c))
        .collect();

    let enabled_all = || {
        fs::read_to_string(&subtree_ctl)
            .map(|cur| {
                wanted
                    .iter()
                    .all(|w| cur.split_whitespace().any(|c| c == *w))
            })
            .unwrap_or(false)
    };
    if enabled_all() {
        return Ok(base);
    }

    let enable = wanted
        .iter()
        .map(|c| format!("+{c}"))
        .collect::<Vec<_>>()
        .join(" ");
    if fs::write(&subtree_ctl, &enable).is_ok() {
        return Ok(base);
    }

    // Base still hosts processes (at least us). Park ourselves — plus
    // our own descendants (e.g. jails spawned earlier without limits,
    // which inherit our cgroup) — in the leaf, then retry. Processes
    // that aren't ours are never moved. The root cgroup is exempt from
    // the no-internal-process rule, so we only get here for a non-root
    // base.
    let leaf = base.join(INIT_LEAF);
    match fs::create_dir(&leaf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(JailError::Cgroup(e)),
    }
    fs::write(leaf.join("cgroup.procs"), pid.to_string()).map_err(JailError::Cgroup)?;
    let move_descendants = || {
        for other in procs_of(&base) {
            if other != pid && is_descendant_of(other, pid) {
                // Best effort: it may have exited meanwhile.
                let _ = fs::write(leaf.join("cgroup.procs"), other.to_string());
            }
        }
    };
    move_descendants();

    // A few retries cover the brief window in which the kernel still
    // counts us as a member of `base` after the migration write.
    let mut last_err = None;
    for _ in 0..20 {
        move_descendants();
        match fs::write(&subtree_ctl, &enable) {
            Ok(()) => return Ok(base),
            Err(e) => last_err = Some(e),
        }
        if enabled_all() {
            return Ok(base);
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let others: Vec<String> = procs_of(&base).iter().map(u32::to_string).collect();
    Err(JailError::Cgroup(std::io::Error::other(format!(
        "cannot enable cgroup controllers ({enable}) on {}: {}; the cgroup still \
         contains other processes [{}] which agentjail will not move. Run agentjail \
         as the only process of a delegated cgroup (e.g. systemd Delegate=yes, or \
         `mkdir <cg>/other && echo <pid> > <cg>/other/cgroup.procs` for the siblings)",
        base.display(),
        last_err.map(|e| e.to_string()).unwrap_or_default(),
        others.join(", "),
    ))))
}

/// PIDs listed in `<cg>/cgroup.procs`.
fn procs_of(cg: &Path) -> Vec<u32> {
    fs::read_to_string(cg.join("cgroup.procs"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// Parent PID from `/proc/<pid>/stat` (field 4, after the `(comm)`).
fn ppid_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &stat[stat.rfind(')')? + 1..];
    after_comm.split_whitespace().nth(1)?.parse().ok()
}

/// Is `pid` a (transitive) child of `ancestor`?
fn is_descendant_of(pid: u32, ancestor: u32) -> bool {
    let mut cur = pid;
    // Bounded walk: PID chains are short, and this guards against races
    // that could produce a cycle while processes exit and PIDs recycle.
    for _ in 0..4096 {
        match ppid_of(cur) {
            Some(p) if p == ancestor => return true,
            Some(0) | Some(1) | None => return false,
            Some(p) => cur = p,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aj-cg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// A freeze that never reaches `frozen 1` must be rolled back so the
    /// caller, which sees `Err`, isn't left holding a frozen jail.
    #[test]
    fn freeze_timeout_rolls_back_to_thawed() {
        let fake = scratch("freeze-timeout");
        fs::write(fake.join("cgroup.freeze"), "0").unwrap();
        fs::write(fake.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();

        let err = freeze_path(&fake, Duration::from_millis(5)).unwrap_err();
        assert!(err.to_string().contains("frozen"), "{err}");
        assert_eq!(
            fs::read_to_string(fake.join("cgroup.freeze"))
                .unwrap()
                .trim(),
            "0",
            "timed-out freeze must write cgroup.freeze=0 before returning Err"
        );
        let _ = fs::remove_dir_all(&fake);
    }

    #[test]
    fn freeze_success_leaves_frozen() {
        let fake = scratch("freeze-ok");
        fs::write(fake.join("cgroup.freeze"), "0").unwrap();
        fs::write(fake.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        freeze_path(&fake, Duration::from_millis(5)).unwrap();
        assert_eq!(fs::read_to_string(fake.join("cgroup.freeze")).unwrap(), "1");
        let _ = fs::remove_dir_all(&fake);
    }

    /// Re-initialising from inside `<base>/agentjail-init` resolves to
    /// `<base>` instead of nesting a new `agentjail-init` level.
    #[test]
    fn init_is_idempotent_and_does_not_nest() {
        let root = scratch("init-idem");
        let base = root.join("svc.slice/app");
        fs::create_dir_all(base.join(INIT_LEAF)).unwrap();
        fs::write(base.join("cgroup.controllers"), "cpu memory pids").unwrap();
        fs::write(base.join("cgroup.subtree_control"), "cpu memory pids").unwrap();

        for our in [
            "/svc.slice/app",
            "/svc.slice/app/agentjail-init",
            "/svc.slice/app/agentjail-init/",
        ] {
            let got = init_cgroup_base_at(&root, our, 1).unwrap();
            assert_eq!(got, base, "our_path={our}");
        }
        assert!(!base.join(INIT_LEAF).join(INIT_LEAF).exists());
        // Already-enabled subtree means no migration was attempted.
        assert!(!base.join(INIT_LEAF).join("cgroup.procs").exists());

        // Root-level parking also maps back to the root.
        fs::write(root.join("cgroup.controllers"), "memory").unwrap();
        fs::write(root.join("cgroup.subtree_control"), "memory").unwrap();
        assert_eq!(
            init_cgroup_base_at(&root, "/agentjail-init", 1).unwrap(),
            root
        );
        let _ = fs::remove_dir_all(&root);
    }

    fn cgroup_of(pid: u32) -> String {
        fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .unwrap_or("")
            .to_string()
    }

    /// Real cgroup2 check (root only): only the calling process is
    /// parked in the leaf; a sibling sharing the cgroup is never moved,
    /// and init errors out instead. Once the sibling is gone, init
    /// succeeds, and a second init from inside the leaf doesn't nest.
    #[test]
    fn init_moves_only_self_and_never_siblings() {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skip: needs root");
            return;
        }
        let root = Path::new(CGROUP_ROOT);
        let ours = cgroup_of(std::process::id());
        // Build the scratch cgroup next to ours: its parent has no
        // processes and already delegates controllers to children.
        let parent_rel = Path::new(&ours)
            .parent()
            .map(|p| p.to_string_lossy().trim_start_matches('/').to_string())
            .unwrap_or_default();
        let t_rel = if parent_rel.is_empty() {
            format!("aj-cgtest-{}", std::process::id())
        } else {
            format!("{parent_rel}/aj-cgtest-{}", std::process::id())
        };
        let t = root.join(&t_rel);
        if fs::create_dir(&t).is_err() {
            eprintln!("skip: cannot create {}", t.display());
            return;
        }

        let mut a = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let mut b = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let (pa, pb) = (a.id(), b.id());
        fs::write(t.join("cgroup.procs"), pa.to_string()).unwrap();
        fs::write(t.join("cgroup.procs"), pb.to_string()).unwrap();
        let t_cg = format!("/{t_rel}");

        // Sibling present → error, sibling untouched, only `a` moved.
        assert!(!is_descendant_of(pb, pa));
        assert!(is_descendant_of(pa, std::process::id()));
        let err = init_cgroup_base_at(root, &t_cg, pa).unwrap_err();
        assert!(err.to_string().contains("will not move"), "{err}");
        assert_eq!(cgroup_of(pb), t_cg, "sibling must not be migrated");
        assert_eq!(cgroup_of(pa), format!("{t_cg}/{INIT_LEAF}"));

        // Sibling gone → success on the same base.
        let _ = b.kill();
        let _ = b.wait();
        let got = init_cgroup_base_at(root, &t_cg, pa).unwrap();
        assert_eq!(got, t);
        let ctl = fs::read_to_string(t.join("cgroup.subtree_control")).unwrap();
        assert!(ctl.contains("pids"), "controllers enabled: {ctl}");

        // Second init from inside the leaf: same base, no nesting.
        let again = init_cgroup_base_at(root, &cgroup_of(pa), pa).unwrap();
        assert_eq!(again, t);
        assert!(!t.join(INIT_LEAF).join(INIT_LEAF).exists());
        assert_eq!(cgroup_of(pa), format!("{t_cg}/{INIT_LEAF}"));

        let _ = a.kill();
        let _ = a.wait();
        for _ in 0..50 {
            if fs::remove_dir(t.join(INIT_LEAF)).is_ok() || !t.join(INIT_LEAF).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = fs::remove_dir(&t);
    }
}
