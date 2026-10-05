//! Snapshot endpoints.
//!
//! - `POST   /v1/workspaces/:id/snapshot`     — capture a named snapshot
//! - `GET    /v1/snapshots`                   — list (optional ?workspace_id=)
//! - `GET    /v1/snapshots/:id`               — detail
//! - `DELETE /v1/snapshots/:id`               — remove
//! - `POST   /v1/workspaces/from-snapshot`    — rehydrate into a new workspace

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{CtlError, Result};
use crate::snapshots::SnapshotRecord;
pub(super) use crate::snapshots::new_snapshot_id;
use crate::tenant::TenantScope;
use crate::workspaces::{Workspace, new_workspace_id};

/// Tenant filter for snapshot list calls: `None` for admins, owned
/// `Some(tenant)` for operators. See `routes::workspaces::tenant_filter`
/// for the same pattern.
fn tenant_filter(scope: &TenantScope) -> Option<String> {
    if scope.role.is_admin() { None } else { Some(scope.tenant.clone()) }
}

use super::AppState;

// ---------- request / response shapes ----------

#[derive(Debug, Deserialize, Default)]
pub(crate) struct CreateSnapshotRequest {
    /// Optional human-readable name for the snapshot.
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotListQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    /// Case-insensitive substring match on `id` / `name` / `workspace_id`.
    #[serde(default)]
    q: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SnapshotList {
    rows: Vec<SnapshotRecord>,
    total: u64,
    limit: usize,
    offset: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FromSnapshotRequest {
    /// The snapshot to rehydrate into the new workspace's output dir.
    snapshot_id: String,
    /// Parent workspace id the caller claims owns the snapshot. The
    /// route refuses to rehydrate when this doesn't match the snapshot's
    /// recorded parent (or when the snapshot's parent is missing —
    /// orphaned snapshots are not addressable here). This is a cheap
    /// ownership check until per-tenant scoping lands.
    parent_workspace_id: String,
    /// Optional label for the new workspace.
    #[serde(default)]
    label: Option<String>,
}

// ---------- handlers ----------

/// `POST /v1/workspaces/:id/snapshot` — captures a snapshot of the
/// workspace's output dir, freezing the running exec (if any) around the
/// copy. This is the *mid-run snapshot* entry point: safe to call during
/// a long-running exec; idle workspaces skip the freeze step.
#[tracing::instrument(
    name = "snapshot.create",
    skip_all,
    fields(
        workspace_id = %id,
        name = req.name.as_deref().unwrap_or(""),
    ),
)]
pub(crate) async fn create_snapshot(
    State(state): State<AppState>,
    scope: TenantScope,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<CreateSnapshotRequest>,
) -> Result<(StatusCode, Json<SnapshotRecord>)> {
    let ws = state.workspaces.get(&id).await
        .filter(|w| scope.can_see(&w.tenant_id))
        .ok_or_else(|| CtlError::NotFound(format!("workspace {id}")))?;

    let snap_id = new_snapshot_id();
    let snap_dir = state.state_dir.join("snapshots").join(&snap_id);

    // Workspace state lives in `source_dir` (mounted at `/workspace`
    // read-write); `output_dir` is the artifact drop zone. Snapshot the
    // dir the jail actually mutates.
    let active = state.active_cgroups.get(&ws.id);
    let (snap, size_bytes) = capture_snapshot(
        active.as_deref(),
        &ws.source_dir,
        &snap_dir,
        state.snapshot_pool_dir.as_deref(),
    )?;

    let record = SnapshotRecord {
        id: snap_id.clone(),
        // Snapshots inherit the parent workspace's tenant — the scope
        // check above makes sure the caller owns that workspace, so
        // stamping ws.tenant_id here can't leak tenancy.
        tenant_id: ws.tenant_id.clone(),
        workspace_id: Some(ws.id.clone()),
        name: req.name,
        created_at: OffsetDateTime::now_utc(),
        path: snap.path().to_path_buf(),
        size_bytes,
    };
    state.snapshots.insert(record.clone()).await.inspect_err(|_| {
        // Undo the on-disk copy if we can't persist the row.
        let _ = std::fs::remove_dir_all(&snap_dir);
    })?;

    Ok((StatusCode::CREATED, Json(record)))
}

/// `GET /v1/snapshots`
pub(crate) async fn list_snapshots(
    State(state): State<AppState>,
    scope: TenantScope,
    Query(q): Query<SnapshotListQuery>,
) -> Json<SnapshotList> {
    let limit  = q.limit.unwrap_or(50).min(500);
    let offset = q.offset.unwrap_or(0);
    let needle = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let t = tenant_filter(&scope);
    let (rows, total) = state
        .snapshots
        .list(t.as_deref(), q.workspace_id.as_deref(), limit, offset, needle)
        .await;
    Json(SnapshotList { rows, total, limit, offset })
}

/// `GET /v1/snapshots/:id`
pub(crate) async fn get_snapshot(
    State(state): State<AppState>,
    scope: TenantScope,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<SnapshotRecord>> {
    state
        .snapshots
        .get(&id)
        .await
        .filter(|s| scope.can_see(&s.tenant_id))
        .map(Json)
        .ok_or_else(|| CtlError::NotFound(format!("snapshot {id}")))
}

/// `GET /v1/snapshots/:id/manifest` — list the files inside a
/// pool-backed snapshot. Returns an empty `entries` array when the
/// snapshot is classic (full-copy) or the manifest is unreadable, so
/// the UI can branch on `kind`.
#[derive(Debug, Serialize)]
pub(crate) struct SnapshotManifest {
    kind: &'static str,
    entries: Vec<ManifestEntryDto>,
}

#[derive(Debug, Serialize)]
struct ManifestEntryDto {
    path: String,
    mode: u32,
    /// Hex-encoded BLAKE3-256 of the file bytes.
    hash: String,
    size: u64,
}

pub(crate) async fn get_snapshot_manifest(
    State(state): State<AppState>,
    scope: TenantScope,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<SnapshotManifest>> {
    let rec = state
        .snapshots
        .get(&id)
        .await
        .filter(|s| scope.can_see(&s.tenant_id))
        .ok_or_else(|| CtlError::NotFound(format!("snapshot {id}")))?;

    match agentjail::load_manifest(&rec.path) {
        Ok(m) => {
            let entries: Vec<ManifestEntryDto> = m
                .entries
                .into_iter()
                .map(|e| ManifestEntryDto {
                    path: e.path, mode: e.mode, hash: e.hash, size: e.size,
                })
                .collect();
            Ok(Json(SnapshotManifest { kind: "incremental", entries }))
        }
        Err(e) => {
            // Could be: (a) classic full-copy snapshot (no manifest on
            // disk — expected, quiet), or (b) a corrupted / unreadable
            // manifest (bug, loud). Distinguish by checking for the
            // specific "not found" case.
            let missing = matches!(&e, agentjail::JailError::Snapshot(io)
                if io.kind() == std::io::ErrorKind::NotFound);
            if !missing {
                tracing::warn!(
                    snapshot_id = %rec.id,
                    path = %rec.path.display(),
                    error = %e,
                    "snapshot manifest unreadable — falling back to classic kind"
                );
            }
            Ok(Json(SnapshotManifest { kind: "classic", entries: Vec::new() }))
        }
    }
}

/// `DELETE /v1/snapshots/:id`
///
/// Removes the snapshot row and its on-disk dir (which for an
/// incremental snapshot is just the `manifest.json`). Blobs in the
/// content-addressed pool remain until the GC sweeper notices they're
/// unreferenced — we never delete them inline because another snapshot
/// may be mid-capture and about to reference the same hash.
pub(crate) async fn delete_snapshot(
    State(state): State<AppState>,
    scope: TenantScope,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode> {
    // Pre-check ownership — remove() would otherwise happily delete
    // a sibling tenant's row.
    let existing = state.snapshots.get(&id).await;
    if !existing.as_ref().is_some_and(|s| scope.can_see(&s.tenant_id)) {
        return Err(CtlError::NotFound(format!("snapshot {id}")));
    }
    let Some(rec) = state.snapshots.remove(&id).await else {
        return Err(CtlError::NotFound(format!("snapshot {id}")));
    };
    if rec.path.exists() {
        if let Err(e) = std::fs::remove_dir_all(&rec.path) {
            tracing::warn!(snapshot_id = %rec.id, error = %e, "snapshot dir cleanup failed");
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/workspaces/from-snapshot` — rehydrate a snapshot into a
/// brand-new workspace. The source dir starts empty (the snapshot lives in
/// the workspace's *output* dir, which is what `live_fork` and
/// `Snapshot::restore_to` write to). Config is inherited from the parent
/// workspace when it's still around.
#[tracing::instrument(
    name = "workspace.from_snapshot",
    skip_all,
    fields(snapshot_id = %req.snapshot_id),
)]
pub(crate) async fn create_workspace_from_snapshot(
    State(state): State<AppState>,
    scope: TenantScope,
    Json(req): Json<FromSnapshotRequest>,
) -> Result<(StatusCode, Json<Workspace>)> {
    let snap = state.snapshots.get(&req.snapshot_id).await
        .filter(|s| scope.can_see(&s.tenant_id))
        .ok_or_else(|| CtlError::NotFound(format!("snapshot {}", req.snapshot_id)))?;

    // Ownership gate. The snapshot's recorded parent must match the
    // parent_workspace_id the caller supplied. We treat the "no match"
    // and "snapshot orphaned" cases as 404 (rather than 403) to avoid
    // leaking which snapshot ids exist.
    let claimed_parent = req.parent_workspace_id.trim();
    if claimed_parent.is_empty() {
        return Err(CtlError::BadRequest("parent_workspace_id is required".into()));
    }
    let recorded_parent = snap.workspace_id.as_deref();
    if recorded_parent != Some(claimed_parent) {
        return Err(CtlError::NotFound(format!("snapshot {}", req.snapshot_id)));
    }
    let parent = state.workspaces.get(claimed_parent).await
        .filter(|p| scope.can_see(&p.tenant_id));
    if parent.is_none() {
        return Err(CtlError::NotFound(format!("snapshot {}", req.snapshot_id)));
    }

    let new_id = new_workspace_id();
    let ws_root = state.state_dir.join("workspaces").join(&new_id);
    let source_dir = ws_root.join("source");
    let output_dir = ws_root.join("output");
    std::fs::create_dir_all(&source_dir).map_err(CtlError::Io)?;
    std::fs::create_dir_all(&output_dir).map_err(CtlError::Io)?;

    // Restore into the new workspace's `source_dir` — that's the
    // writable surface the jail sees at `/workspace`.
    restore_snapshot(
        &snap.path,
        &source_dir,
        state.snapshot_pool_dir.as_deref(),
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&ws_root);
    })?;

    let config = parent
        .as_ref()
        .map(|p| p.config.clone())
        .unwrap_or_else(default_workspace_spec);

    let ws = Workspace {
        id: new_id.clone(),
        // The ownership gate above ensured `parent` exists and belongs
        // to the caller's tenant, so the rehydrated workspace inherits
        // that tenant id — operators can't smuggle snapshots into a
        // different tenant by way of restore.
        tenant_id: parent
            .as_ref()
            .map(|p| p.tenant_id.clone())
            .expect("ownership gate guarantees parent is Some"),
        created_at: OffsetDateTime::now_utc(),
        deleted_at: None,
        source_dir,
        output_dir,
        config,
        git_repo: parent.as_ref().and_then(|p| p.git_repo.clone()),
        git_ref:  parent.as_ref().and_then(|p| p.git_ref.clone()),
        label: req.label
            .and_then(|s| {
                let t = s.trim().to_string();
                if t.is_empty() { None } else { Some(t) }
            })
            .or_else(|| Some(crate::workspaces::slug::generate())),
        domains:       Vec::new(),
        last_exec_at:  None,
        paused_at:     None,
        auto_snapshot: None,
    };
    state.workspaces.insert(ws.clone()).await.inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&ws_root);
    })?;
    // Redact host paths for non-admin callers; admins keep the full view.
    let view = if scope.role.is_admin() {
        ws
    } else {
        Workspace { source_dir: std::path::PathBuf::new(),
                    output_dir: std::path::PathBuf::new(), ..ws }
    };
    Ok((StatusCode::CREATED, Json(view)))
}

// ---------- helpers ----------

/// Capture a snapshot, picking the content-addressed path when a pool
/// dir is configured. Handles freeze-around-copy when an exec is live.
///
/// Returns `(engine_snapshot, reported_size_bytes)`. For incremental
/// snapshots the size is the manifest's logical sum; for full copies
/// it's the actual on-disk footprint.
pub(super) fn capture_snapshot(
    cgroup_path: Option<&std::path::Path>,
    output_dir: &std::path::Path,
    snap_dir: &std::path::Path,
    pool_dir: Option<&std::path::Path>,
) -> Result<(agentjail::Snapshot, u64)> {
    match pool_dir {
        Some(pool) => {
            // Incremental: freeze, hash-into-pool, thaw, write manifest.
            let frozen = cgroup_path.and_then(|p| agentjail::freeze_cgroup(p).ok().map(|()| p));
            let snap = agentjail::Snapshot::create_incremental(output_dir, snap_dir, pool);
            if let Some(p) = frozen {
                let _ = agentjail::thaw_cgroup(p);
            }
            let snap = snap.map_err(CtlError::Jail)?;
            let size = agentjail::load_manifest(snap_dir)
                .map(|m| m.size_bytes())
                .unwrap_or_else(|_| snap.size_bytes());
            Ok((snap, size))
        }
        None => {
            let snap = agentjail::snapshot_frozen(cgroup_path, output_dir, snap_dir)
                .map_err(CtlError::Jail)?;
            let size = snap.size_bytes();
            Ok((snap, size))
        }
    }
}

/// Counterpart to [`capture_snapshot`]. Picks full-vs-incremental based
/// on whether the snapshot dir holds a `manifest.json` (authoritative
/// marker regardless of what the server was started with).
pub(super) fn restore_snapshot(
    snap_dir: &std::path::Path,
    target_dir: &std::path::Path,
    pool_dir: Option<&std::path::Path>,
) -> Result<()> {
    let manifest_path = snap_dir.join("manifest.json");
    if manifest_path.exists() {
        let pool = pool_dir.ok_or_else(|| {
            CtlError::BadRequest(
                "snapshot is content-addressed but AGENTJAIL_SNAPSHOT_POOL_DIR is not set".into(),
            )
        })?;
        agentjail::Snapshot::restore_incremental(snap_dir, pool, target_dir)
            .map_err(CtlError::Jail)
    } else {
        let loaded = agentjail::Snapshot::load(snap_dir, target_dir).map_err(CtlError::Jail)?;
        loaded.restore_to(target_dir).map_err(CtlError::Jail)
    }
}

fn default_workspace_spec() -> crate::workspaces::WorkspaceSpec {
    crate::workspaces::WorkspaceSpec {
        memory_mb:         512,
        timeout_secs:      300,
        cpu_percent:       100,
        max_pids:          64,
        network_mode:      "none".into(),
        network_domains:   Vec::new(),
        seccomp:           "standard".into(),
        idle_timeout_secs: 0,
        flavors:           Vec::new(),
    }
}
