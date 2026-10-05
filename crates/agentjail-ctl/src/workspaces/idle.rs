//! Idle-timeout reaper utilities.
//!
//! Periodically scans live workspaces and pauses any that have been
//! idle longer than their per-workspace `idle_timeout_secs`. Pausing
//! captures a snapshot, wipes the `source_dir`, and marks the workspace
//! with the snapshot id; the next exec auto-restores.

use super::{Workspace, WorkspaceStore};
use crate::snapshots::{SnapshotRecord, SnapshotStore, new_snapshot_id};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::OffsetDateTime;

/// Reaper wiring. All fields are `Arc` / `PathBuf` so the struct can
/// be cloned into a background task cheaply.
#[derive(Clone)]
pub struct IdleReaperConfig {
    /// Workspace store the reaper reads + pauses rows through.
    pub workspaces: Arc<dyn WorkspaceStore>,
    /// Snapshot store used for the auto-snapshot on pause.
    pub snapshots:  Arc<dyn SnapshotStore>,
    /// `<state_dir>/snapshots/<id>/` is where captures land.
    pub state_dir:  PathBuf,
    /// Optional content-addressed pool; when set, auto-snapshots use
    /// the incremental / dedupe path.
    pub pool_dir:   Option<PathBuf>,
    /// How often the reaper runs, in seconds. `0` disables the sweeper.
    pub tick_secs:  u64,
}

/// Run a single reaper pass. Returns the number of workspaces that
/// were paused. Safe to call concurrently with execs because the
/// workspace's `last_exec_at` is re-read under the per-row lock that
/// PG provides; in-memory backends race benignly (worst case: a
/// near-simultaneous exec touches the row right after we snapshot,
/// and the next tick unpauses via the restore path).
pub async fn run_once(cfg: &IdleReaperConfig) -> usize {
    // Idle reaper is a background sweeper — it runs without a tenant
    // scope, admin-style, so every tenant's idle workspaces get paused.
    let (rows, _) = cfg.workspaces.list(None, 500, 0, None).await;
    let now = OffsetDateTime::now_utc();
    let mut paused = 0usize;

    for ws in rows {
        if ws.paused_at.is_some() {
            continue;
        }
        if ws.config.idle_timeout_secs == 0 {
            continue;
        }
        let last = ws.last_exec_at.unwrap_or(ws.created_at);
        let cutoff = now - time::Duration::seconds(ws.config.idle_timeout_secs as i64);
        if last >= cutoff {
            continue;
        }
        match pause_one(cfg, &ws).await {
            Ok(()) => paused += 1,
            Err(e) => {
                tracing::warn!(
                    workspace_id = %ws.id,
                    error = %e,
                    "idle-reaper: pause failed"
                );
            }
        }
    }
    paused
}

/// Spawn a long-running sweeper. Returns `None` when `tick_secs == 0`
/// (the reaper is disabled).
pub fn spawn_sweeper(cfg: IdleReaperConfig) -> Option<tokio::task::JoinHandle<()>> {
    if cfg.tick_secs == 0 {
        return None;
    }
    let tick = std::time::Duration::from_secs(cfg.tick_secs.max(1));
    Some(tokio::spawn(async move {
        let mut iv = tokio::time::interval(tick);
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        iv.tick().await; // skip immediate
        loop {
            iv.tick().await;
            let n = run_once(&cfg).await;
            if n > 0 {
                tracing::info!(paused = n, "workspace idle reaper");
            }
        }
    }))
}

async fn pause_one(cfg: &IdleReaperConfig, ws: &Workspace) -> Result<(), String> {
    let snap_id = new_snapshot_id();
    let snap_dir = cfg.state_dir.join("snapshots").join(&snap_id);

    // Workspace state lives in `source_dir` (the mutable `/workspace`
    // mount); capture it, not the artifact `output_dir`. Freeze isn't
    // needed because the workspace is idle by definition.
    let snap = match cfg.pool_dir.as_deref() {
        Some(pool) => agentjail::Snapshot::create_incremental(&ws.source_dir, &snap_dir, pool)
            .map_err(|e| format!("snapshot create: {e}"))?,
        None => agentjail::Snapshot::create(&ws.source_dir, &snap_dir)
            .map_err(|e| format!("snapshot create: {e}"))?,
    };
    let size_bytes = match cfg.pool_dir.as_deref() {
        Some(_) => agentjail::load_manifest(&snap_dir)
            .map(|m| m.size_bytes())
            .unwrap_or_else(|_| snap.size_bytes()),
        None => snap.size_bytes(),
    };

    let record = SnapshotRecord {
        id: snap_id.clone(),
        // Auto-snapshots inherit the workspace's tenant — they're
        // produced by the reaper, not a human caller, so there's no
        // other tenant context to draw from.
        tenant_id: ws.tenant_id.clone(),
        workspace_id: Some(ws.id.clone()),
        name: Some(format!("auto:{}", ws.id)),
        created_at: OffsetDateTime::now_utc(),
        path: snap.path().to_path_buf(),
        size_bytes,
    };
    if let Err(e) = cfg.snapshots.insert(record).await {
        let _ = std::fs::remove_dir_all(&snap_dir);
        return Err(format!("snapshot insert: {e}"));
    }

    // Wipe source_dir to reclaim disk now that the snapshot is safe.
    // The next exec auto-restores from the snapshot id recorded via
    // `mark_paused`.
    let _ = wipe_dir_contents(&ws.source_dir);

    cfg.workspaces.mark_paused(&ws.id, &snap_id).await;
    Ok(())
}

fn wipe_dir_contents(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            std::fs::remove_file(&p)?;
        } else if ft.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}
