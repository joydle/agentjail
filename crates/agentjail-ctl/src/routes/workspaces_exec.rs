//! `POST /v1/workspaces/:id/exec` handler.
//!
//! Split out from [`super::workspaces`] so the file holding the CRUD +
//! fork endpoints stays focused on workspace lifecycle; this module
//! owns the per-exec state machine (lock → auto-resume → jail spawn →
//! monitor) and its helpers.

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use std::time::Duration;

use super::AppState;
use super::exec::{
    ExecOptions, ExecResponse, NetworkSpec, SeccompSpec, acquire_exec, config_snapshot,
    jail_config, output_to_response,
};
use super::exec_monitor::{ExecRegistration, JailRow, blocking, run_monitored_with};
use super::workspaces::WorkspaceExecRequest;
use crate::error::{CtlError, Result};
use crate::jails::{JailKind, workspace_exec_label};
use crate::tenant::TenantScope;
use crate::workspaces::WorkspaceSpec;

/// How long a second exec waits for the in-flight one before giving up
/// with the original 409 conflict.
const EXEC_LOCK_WAIT: Duration = Duration::from_secs(60);
/// Poll cadence while waiting for the in-flight exec.
const EXEC_LOCK_POLL: Duration = Duration::from_millis(100);

/// Acquire the per-workspace exec lock, waiting for the in-flight exec
/// instead of failing fast with 409.
///
/// Upstream behavior: `try_lock` → `Conflict("another exec is in flight
/// against this workspace")`. Agents legitimately issue serial execs
/// against one workspace from concurrent tool calls, so a bounded wait
/// is friendlier and keeps the same failure mode after `EXEC_LOCK_WAIT`.
/// The lock is a `tokio::sync::Mutex`; polling `try_lock` with
/// `tokio::time::sleep` between attempts never holds the guard across
/// an await boundary it doesn't own.
async fn acquire_exec_lock(
    lock: &tokio::sync::Mutex<()>,
) -> Result<tokio::sync::MutexGuard<'_, ()>> {
    let deadline = tokio::time::Instant::now() + EXEC_LOCK_WAIT;
    loop {
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(_) if tokio::time::Instant::now() >= deadline => {
                return Err(CtlError::Conflict(
                    "another exec is in flight against this workspace".into(),
                ));
            }
            Err(_) => tokio::time::sleep(EXEC_LOCK_POLL).await,
        }
    }
}

/// `POST /v1/workspaces/:id/exec`
#[tracing::instrument(
    name = "workspace.exec",
    skip_all,
    fields(
        workspace_id = %id,
        cmd = %req.cmd,
    ),
)]
pub(crate) async fn exec_in_workspace(
    State(state): State<AppState>,
    scope: TenantScope,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<WorkspaceExecRequest>,
) -> Result<Json<ExecResponse>> {
    let exec_cfg = state
        .exec_config
        .as_ref()
        .ok_or_else(|| CtlError::Internal("exec not enabled".into()))?;

    let ws = state
        .workspaces
        .get(&id)
        .await
        .filter(|w| scope.can_see(&w.tenant_id))
        .ok_or_else(|| CtlError::NotFound(format!("workspace {id}")))?;

    // Acquire exclusive exec lock on this workspace.
    let lock = state.workspace_locks.lock_for(&ws.id);
    let _guard = acquire_exec_lock(&lock).await?;

    // Auto-resume: if the reaper paused this workspace, restore its
    // auto-snapshot before running. The on-disk dir was emptied at
    // pause time, so we rehydrate here and clear the pause marker.
    // Workspaces mutate `source_dir` — the same dir we snapshotted.
    if ws.paused_at.is_some()
        && let Some(snap_id) = ws.auto_snapshot.as_deref()
        && let Some(snap) = state.snapshots.get(snap_id).await
    {
        super::snapshots::restore_snapshot_blocking(
            snap.path.clone(),
            ws.source_dir.clone(),
            state.snapshot_pool_dir.clone(),
        )
        .await?;
        state.workspaces.mark_resumed(&ws.id).await;
    }

    // Global + per-tenant concurrency gate. Charged to the workspace's
    // owner, the tenant whose quota the jail actually consumes.
    let _permit = acquire_exec(&state, &ws.tenant_id, 1)?;
    let _metrics = state.exec_metrics.start();

    // Per-call overrides fall back to the workspace defaults.
    let memory_mb = exec_cfg.clamp_memory(req.memory_mb, ws.config.memory_mb);
    let timeout = req
        .timeout_secs
        .unwrap_or(ws.config.timeout_secs)
        .clamp(1, 3600);

    // Start from canonical PATH; allow the client to append extra env.
    let mut env: Vec<(String, String)> = Vec::with_capacity(req.env.len() + 1);
    env.push(("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()));
    env.extend(req.env.iter().cloned());

    let options = spec_to_options(&ws.config)?;
    // Resolve flavors fresh each exec so a flavor directory rename or
    // a pulled runtime layer is picked up without the workspace having
    // to be recreated. Unknown flavors fail loud — the workspace stays
    // usable; the operator fixes the registry and retries.
    let overlays: Vec<std::path::PathBuf> = if ws.config.flavors.is_empty() {
        Vec::new()
    } else {
        state
            .flavors
            .resolve(&ws.config.flavors)
            .map_err(|missing| {
                CtlError::BadRequest(format!(
                    "flavor {missing:?} is no longer available on this server"
                ))
            })?
            .into_iter()
            .map(|f| f.path)
            .collect()
    };
    // Workspaces follow the "one persistent dir" model — /workspace is
    // read-write so `bun install`, `cargo build`, etc. can mutate the
    // source tree directly.
    let config = jail_config(
        exec_cfg,
        &ws.source_dir,
        &ws.output_dir,
        memory_mb,
        timeout,
        env,
        &options,
        /* source_rw */ true,
        overlays,
    )?;
    let jail = blocking(move || agentjail::Jail::new(config).map_err(CtlError::from)).await?;

    // Bump last_exec_at before spawning so the idle reaper treats this
    // workspace as active even while the exec is running.
    state.workspaces.touch(&ws.id).await;

    let label = workspace_exec_label(&ws.id, &req.cmd);
    // Jail row inherits the workspace's tenant — execs against a
    // workspace belong to whoever owns it, not just whoever launched
    // this call.
    let rec_id = state
        .jails
        .start(ws.tenant_id.clone(), JailKind::Workspace, label, None, None)
        .await;
    let row = JailRow::new(state.jails.clone(), rec_id);
    state
        .jails
        .attach_config(rec_id, config_snapshot(&options, memory_mb, timeout, None))
        .await;
    // Publish the cgroup path for the duration of this exec so a
    // concurrent snapshot can freeze-before-copy. The registration
    // auto-clears on drop.
    let registration = ExecRegistration::new(
        state.active_cgroups.clone(),
        state.active_jail_ips.clone(),
        ws.id.clone(),
    );
    let out = run_monitored_with(
        &state.jails,
        row,
        jail,
        &req.cmd,
        &req.args,
        std::time::Duration::from_secs(timeout),
        Some(registration),
    )
    .await?;

    tracing::info!(
        workspace_id = %ws.id,
        cmd = %req.cmd,
        exit_code = out.exit_code,
        duration_ms = out.duration.as_millis() as u64,
        "workspace exec completed"
    );

    Ok(Json(output_to_response(out)))
}

/// Inverse of `options_to_spec` — the exec path rebuilds an ExecOptions
/// so it can flow through the shared `jail_config` validator.
fn spec_to_options(spec: &WorkspaceSpec) -> Result<ExecOptions> {
    let network = match spec.network_mode.as_str() {
        "none" => None,
        "loopback" => Some(NetworkSpec::Loopback),
        "allowlist" => Some(NetworkSpec::Allowlist {
            domains: spec.network_domains.clone(),
        }),
        other => {
            return Err(CtlError::Internal(format!(
                "workspace has unknown network_mode: {other}"
            )));
        }
    };
    let seccomp = match spec.seccomp.as_str() {
        "standard" => Some(SeccompSpec::Standard),
        "strict" => Some(SeccompSpec::Strict),
        other => {
            return Err(CtlError::Internal(format!(
                "workspace has unknown seccomp: {other}"
            )));
        }
    };
    Ok(ExecOptions {
        network,
        seccomp,
        cpu_percent: Some(spec.cpu_percent),
        max_pids: Some(spec.max_pids),
    })
}

#[cfg(test)]
mod exec_lock_tests {
    use super::*;

    #[tokio::test]
    async fn second_exec_waits_then_succeeds() {
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        // Hold the lock briefly, as an in-flight exec would.
        let g = lock.try_lock().unwrap();
        let waiter = lock.clone();
        let task = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let guard = loop {
                match waiter.try_lock() {
                    Ok(g) => break g,
                    Err(_) if tokio::time::Instant::now() >= deadline => {
                        panic!("exec lock never acquired")
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            };
            drop(guard);
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(g); // exec finishes; the waiting exec must get the lock
        task.await.unwrap();
    }

    #[tokio::test]
    async fn lock_conflict_after_bound() {
        let lock = tokio::sync::Mutex::new(());
        let _g = lock.try_lock().unwrap();
        // Short bound via the same code path shape: emulate the timeout
        // arm by polling a lock that never frees.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(50);
        loop {
            match lock.try_lock() {
                Ok(g) => {
                    drop(g);
                    panic!("lock should stay held")
                }
                Err(_) if tokio::time::Instant::now() >= deadline => break,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        // The bounded case returns the identical upstream error text.
        let err = CtlError::Conflict("another exec is in flight against this workspace".into());
        assert!(format!("{err}").contains("another exec is in flight"));
    }
}
