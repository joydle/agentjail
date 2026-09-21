//! HTTP route handlers, split by resource.
//!
//! Each submodule owns the request/response types, the handler(s), and any
//! helpers that are only used by that resource. Shared execution plumbing
//! (jail config, run_monitored, git_clone, the common request/response
//! types) lives in [`exec`] and is re-used by [`stream`] and [`fork`].

use std::path::PathBuf;
use std::sync::Arc;

use agentjail_phantom::{InMemoryKeyStore, TokenStore};
use axum::Json;
use axum::response::{IntoResponse, Response};

use crate::audit::AuditStore;
use crate::credential::CredentialStore;
use crate::error::CtlError;
use crate::jails::JailStore;
use crate::session::SessionStore;
use crate::flavors::FlavorRegistry;
use crate::snapshots::SnapshotStore;
use crate::workspaces::{ActiveCgroups, ActiveJailIps, WorkspaceLocks, WorkspaceStore};

mod audit;
mod clone_jail;
mod credentials;
mod exec;
mod exec_git;
mod exec_monitor;
mod fork;
mod health;
mod jails;
mod sessions;
mod settings;
mod snapshots;
mod stream;
mod workspaces;
mod workspaces_exec;

pub(crate) use audit::list_audit;
pub(crate) use credentials::{delete_credential, list_credentials, put_credential};
pub(crate) use exec::{create_run, exec_in_session};
pub(crate) use fork::create_fork_run;
pub(crate) use health::{healthz, stats};
pub(crate) use jails::{get_jail, list_jails};
pub(crate) use sessions::{create_session, delete_session, get_session, list_sessions};
pub(crate) use settings::{get_settings, list_flavors, whoami};
pub(crate) use snapshots::{
    create_snapshot, create_workspace_from_snapshot, delete_snapshot, get_snapshot,
    get_snapshot_manifest, list_snapshots,
};
pub(crate) use stream::create_stream_run;
pub(crate) use workspaces::{
    create_workspace, delete_workspace, fork_workspace, get_workspace, list_workspaces,
    patch_workspace,
    reconcile_on_startup as reconcile_workspaces_on_startup,
};
pub(crate) use workspaces_exec::exec_in_workspace;

/// Shared service state passed to every handler.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) tokens: Arc<dyn TokenStore>,
    pub(crate) keys: Arc<InMemoryKeyStore>,
    pub(crate) sessions: Arc<dyn SessionStore>,
    pub(crate) credentials: Arc<dyn CredentialStore>,
    pub(crate) audit: Arc<dyn AuditStore>,
    pub(crate) proxy_base_url: String,
    pub(crate) exec_config: Option<crate::exec::ExecConfig>,
    pub(crate) exec_semaphore: Arc<tokio::sync::Semaphore>,
    pub(crate) exec_metrics: Arc<crate::exec::ExecMetrics>,
    pub(crate) jails: Arc<dyn JailStore>,
    pub(crate) workspaces: Arc<dyn WorkspaceStore>,
    pub(crate) workspace_locks: Arc<WorkspaceLocks>,
    pub(crate) active_cgroups: Arc<ActiveCgroups>,
    /// Live jail-side IP per workspace (populated only when the active
    /// exec used `Network::Allowlist`). Read by the gateway to resolve
    /// `VmPort`-style workspace domains.
    pub(crate) active_jail_ips: Arc<ActiveJailIps>,
    pub(crate) snapshots: Arc<dyn SnapshotStore>,
    /// Root directory for persistent workspace + snapshot data. Always
    /// resolved to an absolute path with `workspaces/` and `snapshots/`
    /// subdirs pre-created.
    pub(crate) state_dir: PathBuf,
    /// Optional content-addressed pool. When `Some`, snapshots dedupe
    /// via SHA-256 hashing instead of full directory copies.
    pub(crate) snapshot_pool_dir: Option<PathBuf>,
    /// Operator-facing platform info surfaced via `GET /v1/config`. All
    /// safe-to-display fields (bind addresses, GC policy, provider
    /// metadata). Never carries secrets.
    pub(crate) platform: Option<crate::PlatformInfo>,
    /// Where jail "flavors" (runtime rootfs layers — nodejs, python,
    /// bun, ...) are resolved. Empty / missing directory = no flavors
    /// available; workspaces that request one are rejected at create.
    pub(crate) flavors: Arc<dyn FlavorRegistry>,
}

impl IntoResponse for CtlError {
    fn into_response(self) -> Response {
        let status = self.status();
        // 4xx messages come from caller-controlled input (BadRequest,
        // NotFound, Conflict, Unauthorized, Config). Safe to surface as-is.
        // 5xx messages can carry host paths, SQL fragments, seccomp
        // failure details — anything that ended up in `Internal` /
        // `Io` / `Jail` / `Phantom`. Log it for operators via
        // `tracing::error`, return a generic string to the client.
        let body = if status.is_server_error() {
            tracing::error!(error = %self, "ctl route returned 5xx");
            Json(serde_json::json!({ "error": "internal server error" }))
        } else {
            Json(serde_json::json!({ "error": self.to_string() }))
        };
        (status, body).into_response()
    }
}
