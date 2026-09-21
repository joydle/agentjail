//! `stdout-heavy` — jail writes 10 MiB to stdout; parent reads all.
//!
//! Primary target of the `tokio::net::unix::pipe::Receiver` migration.
//! Today every read round-trips through tokio's blocking pool
//! ([`pipe.rs`]); expect a large drop once the migration lands.

use super::{Iteration, ScenarioConfig};
use crate::fixtures::Dirs;
use agentjail::{Jail, JailConfig, Network, SeccompLevel};
use anyhow::Result;
use std::time::Instant;

const BYTES: usize = 10 * 1024 * 1024;

pub async fn run(_cfg: &ScenarioConfig) -> Result<Iteration> {
    let dirs = Dirs::fresh("stdout-heavy")?;

    // Raw null bytes from /dev/zero — measures the read-path for
    // binary output, not line framing. Exercises the pipe `Receiver`
    // end-to-end: child writes the 64 KiB pipe full, parent drains
    // via epoll-driven reads.
    let script = format!("head -c {BYTES} < /dev/zero");

    let config = JailConfig {
        source: dirs.source.clone(),
        output: dirs.output.clone(),
        network: Network::None,
        seccomp: SeccompLevel::Disabled,
        landlock: false,
        user_namespace: false,
        pid_namespace: true,
        ipc_namespace: false,
        timeout_secs: 30,
        memory_mb: 0,
        cpu_percent: 0,
        max_pids: 0,
        ..Default::default()
    };
    let jail = Jail::new(config)?;

    let start = Instant::now();
    let out = jail.run("/bin/sh", &["-c", &script]).await?;
    let elapsed = start.elapsed();

    if out.exit_code != 0 {
        anyhow::bail!("producer exited {}", out.exit_code);
    }
    if out.stdout.len() != BYTES {
        anyhow::bail!("expected {} bytes, got {}", BYTES, out.stdout.len());
    }

    Ok(Iteration::ok(
        elapsed,
        serde_json::json!({
            "bytes": BYTES,
            "mb_per_sec": (BYTES as f64 / 1_048_576.0) / elapsed.as_secs_f64(),
        }),
    ))
}
