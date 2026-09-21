//! Live stats sampler.
//!
//! Spawns a background task that reads cgroup v2 metrics directly from
//! disk at a fixed cadence and pushes a `ResourceStats` snapshot through
//! a user-provided callback. The task stops when its returned
//! [`tokio::task::JoinHandle`] is aborted (typically when the jail exits).

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Read a single `u64` from a cgroup file. Returns 0 when the file is
/// missing or unparseable (cgroup controllers occasionally disappear).
fn read_u64(p: PathBuf) -> u64 {
    std::fs::read_to_string(p).ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Parse `cpu.stat` lines like `usage_usec 12345` → 12345.
fn read_cpu_usec(path: &Path) -> u64 {
    let Ok(s) = std::fs::read_to_string(path.join("cpu.stat")) else { return 0 };
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("usage_usec ") {
            return v.trim().parse().unwrap_or(0);
        }
    }
    0
}

/// Parse `io.stat` summed across all devices → `(read_bytes, write_bytes)`.
fn read_io(path: &Path) -> (u64, u64) {
    let Ok(s) = std::fs::read_to_string(path.join("io.stat")) else { return (0, 0) };
    let (mut r, mut w) = (0u64, 0u64);
    for line in s.lines() {
        for kv in line.split_whitespace() {
            if let Some(v) = kv.strip_prefix("rbytes=") { r += v.parse().unwrap_or(0); }
            if let Some(v) = kv.strip_prefix("wbytes=") { w += v.parse().unwrap_or(0); }
        }
    }
    (r, w)
}

/// One stats snapshot read from a cgroup directory.
#[must_use]
pub fn sample(path: &Path) -> agentjail::ResourceStats {
    let (io_read, io_write) = read_io(path);
    agentjail::ResourceStats {
        memory_peak_bytes:    read_u64(path.join("memory.peak")),
        memory_current_bytes: read_u64(path.join("memory.current")),
        cpu_usage_usec:       read_cpu_usec(path),
        oom_killed:           false,
        io_read_bytes:        io_read,
        io_write_bytes:       io_write,
        pids_current:         read_u64(path.join("pids.current")),
    }
}

/// Spawn a tokio task that samples stats at `period` and invokes `cb` with
/// each snapshot. Aborting the returned handle stops the sampler.
pub fn spawn<F>(
    cgroup_path: PathBuf,
    period: Duration,
    mut cb: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnMut(agentjail::ResourceStats) + Send + 'static,
{
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip the immediate tick — the first sample is a moment after spawn.
        interval.tick().await;
        loop {
            interval.tick().await;
            let s = sample(&cgroup_path);
            cb(s);
        }
    })
}
