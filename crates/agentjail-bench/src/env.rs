//! Capture the host environment so JSON results are self-describing.

use serde::Serialize;
use std::fs;

#[derive(Debug, Serialize)]
pub struct EnvInfo {
    pub kernel: String,
    pub cpu_count: usize,
    pub user_namespace: bool,
    pub cgroup_v2: bool,
    pub agentjail_rev: Option<String>,
}

impl EnvInfo {
    pub fn capture() -> Self {
        Self {
            kernel: read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".into()),
            cpu_count: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0),
            user_namespace: has_userns(),
            cgroup_v2: is_cgroup_v2(),
            agentjail_rev: git_rev(),
        }
    }
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn has_userns() -> bool {
    // Kernel-level feature check. Actual CLONE_NEWUSER still may fail
    // (nested containers, Kconfig, distro defaults), so this is advisory.
    fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|n| n > 0)
        .unwrap_or(false)
}

fn is_cgroup_v2() -> bool {
    // cgroup v2 mounts as a single `cgroup2` filesystem at /sys/fs/cgroup.
    // v1 has subsystem-specific dirs (memory, cpu, ...) directly under it.
    fs::read_to_string("/proc/self/mountinfo")
        .map(|s| s.lines().any(|l| l.contains(" cgroup2 ")))
        .unwrap_or(false)
}

fn git_rev() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
