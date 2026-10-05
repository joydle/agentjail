//! Scenario registry.
//!
//! A scenario is `async fn run(&ScenarioConfig) -> Result<Iteration>`
//! that runs one measurable unit of work and returns its latency plus
//! any scenario-specific extras (e.g. which clone method live-fork used).

use anyhow::{Result, anyhow};
use serde_json::Value;
use std::time::Duration;

pub mod fork;
pub mod noop;
pub mod snapshot;
pub mod stdout_heavy;

#[derive(Debug, Clone)]
pub struct ScenarioConfig {
    pub tree_files: usize,
    pub tree_size_kb: usize,
}

/// Result of one measurable iteration.
pub struct Iteration {
    pub ok: bool,
    pub duration_us: u64,
    pub extra: Value,
}

impl Iteration {
    pub fn ok(duration: Duration, extra: Value) -> Self {
        Self {
            ok: true,
            duration_us: duration.as_micros() as u64,
            extra,
        }
    }

    pub fn from_err<E: std::fmt::Display>(err: E) -> Self {
        eprintln!("iteration failed: {err}");
        Self {
            ok: false,
            duration_us: 0,
            extra: Value::Null,
        }
    }
}

/// Dispatch by scenario name. Keep the list here so `main.rs` doesn't
/// need to know the set.
pub async fn dispatch(name: &str, cfg: &ScenarioConfig) -> Result<Iteration> {
    match name {
        "noop" => noop::run(cfg, false).await,
        "noop-full" => noop::run(cfg, true).await,
        "stdout-heavy" => stdout_heavy::run(cfg).await,
        "snapshot-create" => snapshot::create(cfg).await,
        "snapshot-restore" => snapshot::restore(cfg).await,
        "snapshot-repeat" => snapshot::repeat(cfg).await,
        "live-fork" => fork::live_fork(cfg).await,
        other => Err(anyhow!("unknown scenario: {other}")),
    }
}
