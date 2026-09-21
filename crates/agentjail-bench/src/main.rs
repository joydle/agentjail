//! Scenario-driven bench harness for `agentjail`.
//!
//! See `README.md` for usage. Each scenario in `scenarios/` implements
//! one iteration; the runner handles warmup, concurrency, and latency
//! collection.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

mod env;
mod fixtures;
mod metrics;
mod runner;
mod scenarios;

#[derive(Parser, Debug, Clone)]
#[command(about = "Benchmark harness for agentjail")]
struct Cli {
    /// Scenario name — see README for the list.
    scenario: String,

    /// In-flight jails.
    #[arg(long, default_value_t = 1)]
    concurrency: usize,

    /// Total measured iterations.
    #[arg(long, default_value_t = 200)]
    iters: usize,

    /// Warmup iterations (not recorded).
    #[arg(long, default_value_t = 10)]
    warmup: usize,

    /// Write full JSON result to this path.
    #[arg(long)]
    json: Option<PathBuf>,

    /// File count for snapshot scenarios.
    #[arg(long, default_value_t = 1000)]
    tree_files: usize,

    /// Per-file size in KiB for snapshot scenarios.
    #[arg(long, default_value_t = 4)]
    tree_size_kb: usize,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let cfg = scenarios::ScenarioConfig {
        tree_files: cli.tree_files,
        tree_size_kb: cli.tree_size_kb,
    };

    let report = runner::run(
        &cli.scenario,
        cli.concurrency,
        cli.iters,
        cli.warmup,
        &cfg,
    )
    .await
    .with_context(|| format!("scenario {} failed", cli.scenario))?;

    report.print_summary();

    if let Some(path) = cli.json.as_ref() {
        report.write_json(path)?;
        println!("wrote {}", path.display());
    }

    Ok(())
}
