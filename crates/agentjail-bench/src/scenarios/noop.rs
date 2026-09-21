//! `noop` — spawn `/bin/true` and measure wall-clock until exit.
//!
//! This is the lower-bound scenario: the floor of spawn+setup+exec+reap.
//! The 50 ms `wait_for_pid` poll ([`run_internal.rs`]) is the primary
//! contributor at steady state; once that's replaced with `pidfd`,
//! expect this to drop by an order of magnitude.

use super::{Iteration, ScenarioConfig};
use crate::fixtures::Dirs;
use agentjail::{Jail, JailConfig, Network, SeccompLevel};
use anyhow::Result;
use std::time::Instant;

/// `full` = use Standard seccomp + user namespace (when appropriate).
/// Exercises seccomp BPF application on the hot path, letting us see
/// the effect of caching the compiled filter in `Jail::new`.
///
/// Under Docker-privileged root, user_namespace + uid_map is rejected
/// by the kernel — so we mirror the integration tests and only enable
/// user_namespace when running unprivileged.
pub async fn run(_cfg: &ScenarioConfig, full: bool) -> Result<Iteration> {
    let dirs = Dirs::fresh(if full { "noop-full" } else { "noop" })?;

    let is_root = unsafe { libc::getuid() } == 0;

    let config = if full {
        JailConfig {
            source: dirs.source.clone(),
            output: dirs.output.clone(),
            network: Network::None,
            seccomp: SeccompLevel::Standard,
            landlock: false,
            user_namespace: !is_root,
            pid_namespace: true,
            ipc_namespace: true,
            timeout_secs: 10,
            memory_mb: 0,
            cpu_percent: 0,
            max_pids: 0,
            ..Default::default()
        }
    } else {
        JailConfig {
            source: dirs.source.clone(),
            output: dirs.output.clone(),
            network: Network::None,
            seccomp: SeccompLevel::Disabled,
            landlock: false,
            user_namespace: false,
            pid_namespace: true,
            ipc_namespace: false,
            timeout_secs: 10,
            memory_mb: 0,
            cpu_percent: 0,
            max_pids: 0,
            ..Default::default()
        }
    };

    let jail = Jail::new(config)?;

    let start = Instant::now();
    let out = jail.run("/bin/true", &[]).await?;
    let elapsed = start.elapsed();

    if out.exit_code != 0 {
        anyhow::bail!("/bin/true exited {}", out.exit_code);
    }

    Ok(Iteration::ok(elapsed, serde_json::Value::Null))
}
