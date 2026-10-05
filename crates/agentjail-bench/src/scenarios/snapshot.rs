//! Snapshot scenarios.
//!
//! - `snapshot-create`: one-shot `Snapshot::create` over a fresh tree.
//!   Covers the serial walker + `fs::copy` loop.
//!
//! - `snapshot-restore`: `Snapshot::restore` from a previously-captured
//!   snapshot. Same walker cost, opposite direction.
//!
//! - `snapshot-repeat`: second `create_incremental` against an already-
//!   populated pool. Exercises the `(path, size, mtime_ns)` fast-path
//!   introduced in manifest v2 — unchanged files are reused without a
//!   fresh BLAKE3 hash.
//!
//! The tree size is controlled by `--tree-files` / `--tree-size-kb`.
//! Default (1000 × 4 KiB = ~4 MiB, 1000 files) is tuned to be
//! syscall-bound rather than bandwidth-bound.

use super::{Iteration, ScenarioConfig};
use crate::fixtures::{Dirs, fabricate_tree};
use agentjail::{Snapshot, load_manifest};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static SNAP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn snap_dir(tag: &str) -> PathBuf {
    let n = SNAP_COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!(
        "/tmp/aj-bench-snap-{}-{}-{}",
        std::process::id(),
        tag,
        n
    ))
}

pub async fn create(cfg: &ScenarioConfig) -> Result<Iteration> {
    let dirs = Dirs::fresh("snapshot-create")?;
    let bytes = fabricate_tree(&dirs.output, cfg.tree_files, cfg.tree_size_kb)?;

    let snap = snap_dir("create");
    let snap_for_cleanup = snap.clone();

    // Running in a blocking task — Snapshot::create is synchronous and
    // does a directory walk. Don't starve the multi-threaded runtime.
    let start = Instant::now();
    let elapsed = tokio::task::spawn_blocking(move || -> Result<_> {
        Snapshot::create(&dirs.output, &snap)?;
        Ok(start.elapsed())
    })
    .await??;

    let _ = std::fs::remove_dir_all(&snap_for_cleanup);

    Ok(Iteration::ok(
        elapsed,
        serde_json::json!({ "bytes": bytes, "files": cfg.tree_files }),
    ))
}

pub async fn restore(cfg: &ScenarioConfig) -> Result<Iteration> {
    let dirs = Dirs::fresh("snapshot-restore")?;
    let bytes = fabricate_tree(&dirs.output, cfg.tree_files, cfg.tree_size_kb)?;

    let snap_path = snap_dir("restore");
    let snap = Snapshot::create(&dirs.output, &snap_path)?;

    let start = Instant::now();
    let elapsed = tokio::task::spawn_blocking(move || -> Result<_> {
        snap.restore()?;
        Ok(start.elapsed())
    })
    .await??;

    let _ = std::fs::remove_dir_all(&snap_path);

    Ok(Iteration::ok(
        elapsed,
        serde_json::json!({ "bytes": bytes, "files": cfg.tree_files }),
    ))
}

pub async fn repeat(cfg: &ScenarioConfig) -> Result<Iteration> {
    let dirs = Dirs::fresh("snapshot-repeat")?;
    let bytes = fabricate_tree(&dirs.output, cfg.tree_files, cfg.tree_size_kb)?;

    let pool = snap_dir("pool");
    let snap1 = snap_dir("inc1");
    let snap2 = snap_dir("inc2");
    let pool_for_cleanup = pool.clone();
    let snap1_for_cleanup = snap1.clone();
    let snap2_for_cleanup = snap2.clone();

    let output = dirs.output.clone();
    let elapsed = tokio::task::spawn_blocking(move || -> Result<_> {
        // Prime the pool with the first incremental snapshot — this is
        // *not* measured. The measurement is the *second* pass, where
        // every blob is already in the pool AND we feed the prior
        // manifest as the mtime+size hint so the walker skips hashing.
        Snapshot::create_incremental(&output, &snap1, &pool)?;
        let prior = load_manifest(&snap1)?;

        let start = Instant::now();
        Snapshot::create_incremental_with_hint(&output, &snap2, &pool, Some(&prior))?;
        Ok(start.elapsed())
    })
    .await??;

    let _ = std::fs::remove_dir_all(&pool_for_cleanup);
    let _ = std::fs::remove_dir_all(&snap1_for_cleanup);
    let _ = std::fs::remove_dir_all(&snap2_for_cleanup);

    Ok(Iteration::ok(
        elapsed,
        serde_json::json!({
            "bytes": bytes,
            "files": cfg.tree_files,
            "mode": "incremental-repeat-with-hint",
        }),
    ))
}
