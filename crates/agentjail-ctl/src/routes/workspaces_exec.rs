//! `POST /v1/workspaces/:id/exec` handler.
//!
//! Split out from [`super::workspaces`] so the file holding the CRUD +
//! fork endpoints stays focused on workspace lifecycle; this module
//! owns the per-exec state machine (lock → auto-resume → jail spawn →
//! monitor) and its helpers.

use axum::Json;
use axum::extract::{Path as AxumPath, State};

use super::AppState;
use super::exec::{
    ExecOptions, ExecResponse, NetworkSpec, SeccompSpec, config_snapshot, jail_config,
    output_to_response,
};
use super::exec_monitor::{ExecRegistration, run_monitored_with};
use super::workspaces::WorkspaceExecRequest;
use crate::error::{CtlError, Result};
use crate::jails::{JailKind, workspace_exec_label};
use crate::tenant::TenantScope;
use crate::workspaces::WorkspaceSpec;

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
    if state.exec_config.is_none() {
        return Err(CtlError::Internal("exec not enabled".into()));
    }

    let ws = state.workspaces.get(&id).await
        .filter(|w| scope.can_see(&w.tenant_id))
        .ok_or_else(|| CtlError::NotFound(format!("workspace {id}")))?;

    // Acquire exclusive exec lock on this workspace.
    let lock = state.workspace_locks.lock_for(&ws.id);
    let _guard = lock.try_lock()
        .map_err(|_| CtlError::Conflict(
            "another exec is in flight against this workspace".into(),
        ))?;

    // Auto-resume: if the reaper paused this workspace, restore its
    // auto-snapshot before running. The on-disk dir was emptied at
    // pause time, so we rehydrate here and clear the pause marker.
    // Workspaces mutate `source_dir` — the same dir we snapshotted.
    if ws.paused_at.is_some()
        && let Some(snap_id) = ws.auto_snapshot.as_deref()
        && let Some(snap) = state.snapshots.get(snap_id).await
    {
        super::snapshots::restore_snapshot(
            &snap.path,
            &ws.source_dir,
            state.snapshot_pool_dir.as_deref(),
        )?;
        state.workspaces.mark_resumed(&ws.id).await;
    }

    // Global concurrency gate on the ctl crate.
    let _permit = state.exec_semaphore.try_acquire()
        .map_err(|_| CtlError::BadRequest("too many concurrent executions".into()))?;
    let _metrics = state.exec_metrics.start();

    // Per-call overrides fall back to the workspace defaults.
    let memory_mb = req.memory_mb
        .unwrap_or(ws.config.memory_mb)
        .min(8192);
    let timeout = req.timeout_secs
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
        state.flavors
            .resolve(&ws.config.flavors)
            .map_err(|missing| CtlError::BadRequest(
                format!("flavor {missing:?} is no longer available on this server"),
            ))?
            .into_iter()
            .map(|f| f.path)
            .collect()
    };
    // Workspaces follow the "one persistent dir" model — /workspace is
    // read-write so `bun install`, `cargo build`, etc. can mutate the
    // source tree directly.
    let config = jail_config(
        &ws.source_dir, &ws.output_dir, memory_mb, timeout, env, &options,
        /* source_rw */ true,
        overlays,
    )?;
    let jail = agentjail::Jail::new(config)?;
    let args_refs: Vec<&str> = req.args.iter().map(|s| s.as_str()).collect();

    // Bump last_exec_at before spawning so the idle reaper treats this
    // workspace as active even while the exec is running.
    state.workspaces.touch(&ws.id).await;

    let label = workspace_exec_label(&ws.id, &req.cmd);
    // Jail row inherits the workspace's tenant — execs against a
    // workspace belong to whoever owns it, not just whoever launched
    // this call.
    let rec_id = state.jails
        .start(ws.tenant_id.clone(), JailKind::Workspace, label, None, None)
        .await;
    state.jails.attach_config(
        rec_id,
        config_snapshot(&options, memory_mb, timeout, None),
    ).await;
    // Publish the cgroup path for the duration of this exec so a
    // concurrent snapshot can freeze-before-copy. The registration
    // auto-clears on drop.
    let registration = ExecRegistration::new(
        state.active_cgroups.clone(),
        state.active_jail_ips.clone(),
        ws.id.clone(),
    );
    let result = run_monitored_with(
        &state.jails, rec_id, &jail, &req.cmd, &args_refs, Some(registration),
    ).await;
    let out = match result {
        Ok(o)  => { state.jails.finish(rec_id, &o).await; o }
        Err(e) => { state.jails.error(rec_id, e.to_string()).await; return Err(e.into()); }
    };

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
        "none"      => None,
        "loopback"  => Some(NetworkSpec::Loopback),
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
        "strict"   => Some(SeccompSpec::Strict),
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
        max_pids:    Some(spec.max_pids),
    })
}
