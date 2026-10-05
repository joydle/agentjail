//! `live-fork` — clone a running jail's output dir.
//!
//! The measurement is just the `live_fork` call itself. We record the
//! `clone_method` (Reflink/Mixed/Copy) as an extra so regressions that
//! silently disable FICLONE are visible.

use super::{Iteration, ScenarioConfig};
use crate::fixtures::{Dirs, fabricate_tree};
use agentjail::{Jail, JailConfig, Network, SeccompLevel};
use anyhow::Result;
use std::path::PathBuf;
use std::time::Instant;

pub async fn live_fork(cfg: &ScenarioConfig) -> Result<Iteration> {
    let dirs = Dirs::fresh("live-fork")?;
    let _bytes = fabricate_tree(&dirs.output, cfg.tree_files, cfg.tree_size_kb)?;

    // Write a long-running script so we have a live jail to fork from.
    std::fs::write(
        dirs.source.join("sleep.sh"),
        "#!/bin/sh\nsleep 60\n",
    )?;

    let config = JailConfig {
        source: dirs.source.clone(),
        output: dirs.output.clone(),
        network: Network::None,
        seccomp: SeccompLevel::Disabled,
        landlock: false,
        user_namespace: false,
        pid_namespace: true,
        ipc_namespace: false,
        timeout_secs: 120,
        // Need a cgroup if we want the freezer path exercised. Empty
        // limits would skip cgroup creation entirely.
        memory_mb: 128,
        cpu_percent: 100,
        max_pids: 32,
        ..Default::default()
    };

    let jail = Jail::new(config)?;
    let handle = jail.spawn("/bin/sh", &["/workspace/sleep.sh"])?;

    let fork_out: PathBuf = PathBuf::from(format!(
        "/tmp/aj-bench-fork-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));

    let start = Instant::now();
    let (_forked, info) = jail.live_fork(Some(&handle), &fork_out)?;
    let elapsed = start.elapsed();

    // Kill the source jail — we're not measuring its wait.
    handle.kill();

    let _ = std::fs::remove_dir_all(&fork_out);

    Ok(Iteration::ok(
        elapsed,
        serde_json::json!({
            "clone_method": format!("{:?}", info.clone_method),
            "files_cloned": info.files_cloned,
            "files_cow": info.files_cow,
            "bytes_cloned": info.bytes_cloned,
            "was_frozen": info.was_frozen,
        }),
    ))
}
