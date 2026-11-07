//! Live-fork — `POST /v1/runs/fork`.
//!
//! Spawn a parent, COW-clone the output mid-run, then spawn one or more
//! children against the forked state. Children run in parallel.
//!
//! The request accepts either legacy `child_code` (single child) or
//! `children: [{code,...}]` (N children). The response mirrors the shape:
//! single-child calls get `child` + `fork`; multi-child calls get
//! `children` + `forks`. Legacy fields stay populated for the first child
//! so existing SDKs keep working.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::error::{CtlError, Result};
use crate::jails::JailKind;
use crate::sampler;

use super::AppState;
use super::exec::{
    ExecOptions, ExecResponse, GitSpec, acquire_exec, config_snapshot, default_language,
    jail_config, language_runtime, output_to_response, write_code_file,
};
use super::exec_git::git_clone;
use super::exec_monitor::{AbortOnDrop, JailRow, blocking, run_monitored, spawn_jail};

#[derive(Debug, Deserialize)]
pub(crate) struct ForkRequest {
    parent_code: String,
    #[serde(default)]
    child_code: Option<String>,
    #[serde(default)]
    children: Vec<ForkChild>,
    #[serde(default = "default_language")]
    language: String,
    /// How long the parent runs before we freeze + fork. Default 1500ms.
    #[serde(default)]
    fork_after_ms: Option<u64>,
    timeout_secs: Option<u64>,
    memory_mb: Option<u64>,
    #[serde(flatten)]
    options: ExecOptions,
    #[serde(default)]
    git: Option<GitSpec>,
}

/// One child in an N-way fork.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct ForkChild {
    code: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ForkResponse {
    parent: ExecResponse,
    /// First child (back-compat). Identical to `children[0]`.
    child: ExecResponse,
    /// All children in invocation order.
    children: Vec<ExecResponse>,
    /// ForkMeta for the first child (back-compat).
    fork: ForkMeta,
    /// Per-child ForkMeta.
    forks: Vec<ForkMeta>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ForkMeta {
    clone_ms: u64,
    files_cloned: u64,
    files_cow: u64,
    bytes_cloned: u64,
    method: String,
    was_frozen: bool,
}

impl From<agentjail::ForkInfo> for ForkMeta {
    fn from(f: agentjail::ForkInfo) -> Self {
        Self {
            clone_ms: u64::try_from(f.clone_duration.as_millis()).unwrap_or(u64::MAX),
            files_cloned: f.files_cloned,
            files_cow: f.files_cow,
            bytes_cloned: f.bytes_cloned,
            method: format!("{:?}", f.clone_method).to_lowercase(),
            was_frozen: f.was_frozen,
        }
    }
}

/// HTTP handler — thin wrapper. Delegates to [`ForkOrch::run`] and,
/// if that bails partway through, calls [`ForkOrch::fail_stragglers`]
/// to mark any jail rows that were started but never completed. Without
/// that step, an error between "start jail row" and "finish/error jail
/// row" left rows stuck in `running` forever.
pub(crate) async fn create_fork_run(
    State(state): State<AppState>,
    scope: crate::tenant::TenantScope,
    Json(req): Json<ForkRequest>,
) -> Result<(StatusCode, Json<ForkResponse>)> {
    let mut orch = ForkOrch::new(&state, scope.tenant.clone());
    match orch.run(req).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            orch.fail_stragglers(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Normalized + validated shape of a [`ForkRequest`]. Produced once up
/// front so the run loop doesn't re-do the limit math at each step.
struct ForkPlan {
    children: Vec<ForkChild>,
    timeout: u64,
    memory: u64,
    fork_after: u64,
}

/// Orchestrator for a single fork run. Owns the set of jail-record ids
/// it has started so the HTTP handler can mark any that never reached
/// `finish`/`error` as failed on the way out.
///
/// Not a formal state machine: the flow is strictly linear (validate →
/// spawn parent → wait → fork N → run → collect). The struct exists to
/// give us a cleanup hook, not to model transitions.
struct ForkOrch<'s> {
    state: &'s AppState,
    /// Tenant of the caller — stamped on every jail row this fork
    /// creates (parent + children) so operator queries see only their
    /// own runs.
    tenant: String,
    /// Rows started but not yet handed to something that finalises
    /// them. Each is a [`JailRow`] guard, so even a cancelled request
    /// (orchestrator dropped) records them as aborted.
    parent_row: Option<JailRow>,
    child_rows: Vec<JailRow>,
}

impl<'s> ForkOrch<'s> {
    fn new(state: &'s AppState, tenant: String) -> Self {
        Self {
            state,
            tenant,
            parent_row: None,
            child_rows: Vec::new(),
        }
    }

    /// Validate and normalize the request into a [`ForkPlan`]. Pulled
    /// out so the run loop is shorter and the limits are trivially
    /// testable without spinning up a jail.
    fn validate(req: &ForkRequest, exec_cfg: &crate::exec::ExecConfig) -> Result<ForkPlan> {
        // Normalize: either `children: [...]` or fall back to the legacy
        // single `child_code`. At least one child is required.
        let mut children: Vec<ForkChild> = req.children.clone();
        if children.is_empty() {
            let Some(c) = req.child_code.clone() else {
                return Err(CtlError::BadRequest(
                    "children or child_code is required".into(),
                ));
            };
            children.push(ForkChild { code: c });
        }
        if children.len() > 16 {
            return Err(CtlError::BadRequest("at most 16 children per fork".into()));
        }
        if req.parent_code.len() > 1024 * 1024
            || children.iter().any(|c| c.code.len() > 1024 * 1024)
        {
            return Err(CtlError::BadRequest("code exceeds 1 MB".into()));
        }
        Ok(ForkPlan {
            children,
            timeout: req
                .timeout_secs
                .unwrap_or(exec_cfg.default_timeout_secs)
                .clamp(1, 3600),
            memory: exec_cfg.clamp_memory(req.memory_mb, exec_cfg.default_memory_mb),
            fork_after: req.fork_after_ms.unwrap_or(1500).min(30_000),
        })
    }

    /// Mark every jail row we started but never finalized as errored.
    /// Safe to call on the happy path too — it no-ops for finalized rows.
    async fn fail_stragglers(&mut self, msg: &str) {
        if let Some(r) = self.parent_row.take() {
            r.error(msg.to_string()).await;
        }
        for r in self.child_rows.drain(..) {
            r.error(msg.to_string()).await;
        }
    }

    async fn run(&mut self, req: ForkRequest) -> Result<(StatusCode, Json<ForkResponse>)> {
        let exec_cfg = self
            .state
            .exec_config
            .as_ref()
            .ok_or_else(|| CtlError::Internal("exec not enabled".into()))?;
        let plan = Self::validate(&req, exec_cfg)?;

        // One permit per jail: the parent plus every forked child.
        let _permit = acquire_exec(self.state, &self.tenant, plan.children.len() + 1)?;
        let _guard = self.state.exec_metrics.start();

        let (filename, cmd) = language_runtime(&req.language)?;

        let source_dir = tempfile::tempdir().map_err(CtlError::Io)?;
        let parent_out = tempfile::tempdir().map_err(CtlError::Io)?;

        if let Some(g) = &req.git {
            git_clone(g, source_dir.path()).await?;
        }

        // Write parent + each child file into the same source dir so the
        // forked jails all see them via /workspace.
        write_code_file(source_dir.path(), filename, &req.parent_code)?;
        for (i, c) in plan.children.iter().enumerate() {
            let name = format!("child_{i}_{filename}");
            write_code_file(source_dir.path(), &name, &c.code)?;
        }

        let run_env = vec![("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into())];
        let parent_config = jail_config(
            exec_cfg,
            source_dir.path(),
            parent_out.path(),
            plan.memory,
            plan.timeout,
            run_env.clone(),
            &req.options,
            /* source_rw */ false,
            Vec::new(),
        )?;

        let parent_rec = self
            .state
            .jails
            .start(
                self.tenant.clone(),
                JailKind::Fork,
                format!("{} · parent", req.language),
                None,
                None,
            )
            .await;
        let run_config = config_snapshot(&req.options, plan.memory, plan.timeout, req.git.as_ref());
        self.state
            .jails
            .attach_config(parent_rec, run_config.clone())
            .await;
        // Guard: if this request is cancelled, the row is still finalised.
        self.parent_row = Some(JailRow::new(self.state.jails.clone(), parent_rec));
        let (parent_jail, parent_handle) = spawn_jail(
            parent_config,
            cmd.to_string(),
            vec![format!("/workspace/{filename}")],
        )
        .await?;
        let timeout = std::time::Duration::from_secs(plan.timeout);
        let parent_deadline = tokio::time::Instant::now() + timeout;

        let parent_sampler = parent_handle.cgroup_path().map(|p| {
            let jails = self.state.jails.clone();
            AbortOnDrop::new(sampler::spawn(
                p,
                std::time::Duration::from_millis(500),
                move |s| {
                    let jails = jails.clone();
                    tokio::spawn(async move {
                        jails.sample_stats(parent_rec, &s).await;
                    });
                },
            ))
        });

        // Let the parent do its setup (write checkpoint, etc.) before we fork.
        tokio::time::sleep(std::time::Duration::from_millis(plan.fork_after)).await;

        // Fork once per child — each gets its own COW clone of the parent
        // output. We keep each child's output tempdir alive for the run.
        // `live_fork` freezes the parent and COW-copies its output dir:
        // filesystem-bound work that runs on the blocking pool.
        let mut child_outs: Vec<tempfile::TempDir> = Vec::with_capacity(plan.children.len());
        for _ in &plan.children {
            child_outs.push(tempfile::tempdir().map_err(CtlError::Io)?);
        }
        let out_paths: Vec<std::path::PathBuf> =
            child_outs.iter().map(|d| d.path().to_path_buf()).collect();
        let (parent_jail, parent_handle, child_jails, forks_meta) = blocking(move || {
            let mut jails = Vec::with_capacity(out_paths.len());
            let mut metas: Vec<ForkMeta> = Vec::with_capacity(out_paths.len());
            for p in &out_paths {
                let (jail, info) = parent_jail.live_fork(Some(&parent_handle), p)?;
                jails.push(jail);
                metas.push(info.into());
            }
            Ok((parent_jail, parent_handle, jails, metas))
        })
        .await?;

        // Record child rows (each linked to parent_rec).
        for i in 0..plan.children.len() {
            let label = if plan.children.len() == 1 {
                format!("{} · child", req.language)
            } else {
                format!("{} · child {}", req.language, i)
            };
            let id = self
                .state
                .jails
                .start(
                    self.tenant.clone(),
                    JailKind::Fork,
                    label,
                    None,
                    Some(parent_rec),
                )
                .await;
            self.state.jails.attach_config(id, run_config.clone()).await;
            self.child_rows
                .push(JailRow::new(self.state.jails.clone(), id));
        }

        // Run all children in parallel alongside the parent's wait.
        // `run_monitored` finalises each child row itself.
        let child_rows = std::mem::take(&mut self.child_rows);
        let child_futures: Vec<_> = child_jails
            .into_iter()
            .zip(child_rows)
            .enumerate()
            .map(|(i, (jail, row))| {
                let jails = self.state.jails.clone();
                let name = format!("/workspace/child_{i}_{filename}");
                async move { run_monitored(&jails, row, jail, cmd, &[name], timeout).await }
            })
            .collect();

        // The parent's wait is bounded by its own deadline (+ grace).
        let parent_wait = async {
            let wait_deadline = parent_deadline + std::time::Duration::from_secs(5);
            match tokio::time::timeout_at(wait_deadline, parent_handle.wait()).await {
                Ok(r) => r.map_err(CtlError::from),
                Err(_) => Err(CtlError::Internal(
                    "parent jail did not exit after timeout".into(),
                )),
            }
        };
        let (parent_res, child_ress) =
            tokio::join!(parent_wait, futures::future::join_all(child_futures),);
        drop(parent_sampler);
        drop(parent_jail);

        // Collect results. Every child row is already finalised; on the
        // first failure we return early.
        let parent_row = self.parent_row.take().expect("parent row set above");
        let parent_output = match parent_res {
            Ok(r) => {
                parent_row.finish(&r).await;
                r
            }
            Err(e) => {
                parent_row.error(e.to_string()).await;
                return Err(e);
            }
        };
        let mut child_outputs: Vec<agentjail::Output> = Vec::with_capacity(child_ress.len());
        for r in child_ress {
            child_outputs.push(r?);
        }

        drop(source_dir);
        drop(parent_out);
        drop(child_outs);

        tracing::info!(
            language    = %req.language,
            parent_exit = parent_output.exit_code,
            children    = child_outputs.len(),
            "fork run completed"
        );

        let all: Vec<ExecResponse> = child_outputs.into_iter().map(output_to_response).collect();
        let first_child = all[0].clone();
        let first_fork = forks_meta[0].clone();

        Ok((
            StatusCode::CREATED,
            Json(ForkResponse {
                parent: output_to_response(parent_output),
                child: first_child,
                children: all,
                fork: first_fork,
                forks: forks_meta,
            }),
        ))
    }
}
