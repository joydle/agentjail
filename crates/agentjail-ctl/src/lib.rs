//! # agentjail-ctl
//!
//! HTTP control plane for `agentjail` + `agentjail-phantom`.
//!
//! Provides a small REST surface for:
//! - Attaching real upstream credentials (OpenAI, Anthropic, ...).
//! - Creating *sessions* that return a set of phantom-token env vars to
//!   hand to a sandbox.
//! - Listing the phantom-proxy audit log.
//!
//! The jail-execution lifecycle itself is pluggable and intentionally out
//! of scope here — this crate is the credential-broker / session-keeper
//! layer. Run it standalone in front of your existing sandbox, or pair it
//! with `agentjail` for a full platform.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use agentjail_ctl::{ControlPlane, ControlPlaneConfig};
//! use agentjail_phantom::{InMemoryTokenStore, InMemoryKeyStore};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let tokens = Arc::new(InMemoryTokenStore::new());
//! let keys = Arc::new(InMemoryKeyStore::new());
//! let ctl = ControlPlane::new(ControlPlaneConfig {
//!     tokens,
//!     keys,
//!     proxy_base_url: "http://10.0.0.1:8443".into(),
//!     api_keys: vec![],
//!     exec: None,
//!     state_dir: None,
//!     snapshot_pool_dir: None,
//!     platform: None,
//!     active_jail_ips: None,
//! });
//! let router = ctl.router();
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:7000").await?;
//! axum::serve(listener, router).await?;
//! # Ok(()) }
//! ```

#![warn(missing_docs)]

mod audit;
mod auth;
mod credential;
pub(crate) mod db;
mod error;
mod exec;
mod flavors;
mod jails;
mod routes;
mod sampler;
mod session;
mod snapshots;
mod tenant;
mod workspaces;

use std::path::PathBuf;
use std::sync::Arc;

use agentjail_phantom::{InMemoryKeyStore, TokenStore};
use axum::routing::{delete, get, post};
use axum::{Router, middleware};
use routes::AppState;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

pub use audit::{AuditRow, AuditStore, AuditStoreSink, InMemoryAuditStore};
pub use db::{
    PgAuditStore, PgCredentialStore, PgJailStore, PgSessionStore, PgSnapshotStore,
    PgTokenStore, PgWorkspaceStore,
};
pub use auth::ApiKeys;
pub use credential::{CredentialRecord, CredentialStore, InMemoryCredentialStore};
pub use error::{CtlError, Result};
pub use exec::{ExecConfig, ExecMetrics};
pub use jails::{InMemoryJailStore, JailConfigSnapshot, JailKind, JailRecord, JailStatus, JailStore};
pub use session::{InMemorySessionStore, Session, SessionStore};
pub use snapshots::{
    InMemorySnapshotStore, SnapshotGcConfig, SnapshotRecord, SnapshotStore, gc as snapshot_gc,
};
pub use flavors::{DirFlavorRegistry, Flavor, FlavorRegistry};
pub use workspaces::{
    ActiveCgroups, ActiveJailIps, InMemoryWorkspaceStore, Workspace, WorkspaceDomain,
    WorkspaceDomainTarget, WorkspaceLocks, WorkspaceSpec, WorkspaceStore,
    idle as workspace_idle,
};

/// Configuration for a [`ControlPlane`].
pub struct ControlPlaneConfig {
    /// Underlying phantom token store. Share this with the phantom proxy.
    pub tokens: Arc<dyn TokenStore>,
    /// Real-keys store. Share this with the phantom proxy.
    pub keys: Arc<InMemoryKeyStore>,
    /// Base URL the sandbox uses to reach the phantom proxy.
    ///
    /// Example: `"http://10.0.0.1:8443"`. Must start with `http://` or
    /// `https://` and must not end with `/`. Call
    /// [`ControlPlaneConfig::validate`] to surface bad values up-front;
    /// the constructors accept any string and only normalize the trailing
    /// slash defensively.
    pub proxy_base_url: String,
    /// API keys accepted by the control plane. Empty list disables auth
    /// (useful only for dev and tests).
    pub api_keys: Vec<String>,
    /// Jail execution config. When set, enables `/v1/sessions/:id/exec`
    /// and `/v1/runs`. When `None`, those endpoints return 501.
    pub exec: Option<ExecConfig>,
    /// Root directory for persistent workspace + snapshot data. Created
    /// on startup if missing. Per-resource layout:
    /// `<state_dir>/workspaces/<id>/{source,output}` and
    /// `<state_dir>/snapshots/<id>/`.
    ///
    /// Falls back to `std::env::temp_dir().join("agentjail-state")` when
    /// unset — fine for dev, not for production (tmpfs is wiped at boot).
    pub state_dir: Option<PathBuf>,
    /// When set, snapshots are captured into a shared content-addressed
    /// object pool rooted here (rather than as standalone directory
    /// copies). Enables dedupe across snapshots + near-free restores via
    /// hardlink.
    pub snapshot_pool_dir: Option<PathBuf>,
    /// Read-only operator-facing platform info exposed via
    /// `GET /v1/config`. Caller-populated; `None` = the endpoint still
    /// works but returns stub values for the platform fields.
    pub platform: Option<PlatformInfo>,
    /// Live jail-IP registry, shared with the hostname-routed gateway
    /// listener. Populated by workspace execs with allowlist network;
    /// read by the gateway to resolve `vm_port` workspace domains to
    /// `http://<ip>:<port>/` at request time. `None` = caller doesn't
    /// run the gateway, so no sharing is needed.
    pub active_jail_ips: Option<Arc<ActiveJailIps>>,
}

/// Phantom-proxy provider registered with the running server. Exposed
/// via [`crate::routes::settings`] so operators can confirm which
/// upstreams their sandboxes can reach. All fields are safe to surface;
/// real credentials never appear here.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProviderInfo {
    /// Stable id the SDK/clients use (e.g. `"openai"`).
    pub service_id: String,
    /// Upstream host the phantom proxy forwards to (e.g.
    /// `"https://api.openai.com"`).
    pub upstream_base: String,
    /// Public path prefix sandboxes hit (e.g. `"/v1/openai/"`).
    pub request_prefix: String,
}

/// Extra operator-facing settings the server injects into the control
/// plane so `GET /v1/config` can surface them. None of these ship
/// credentials — addresses, thresholds, and provider metadata only.
#[derive(Clone, Debug, Default)]
pub struct PlatformInfo {
    /// Phantom-proxy providers registered at startup.
    pub providers: Vec<ProviderInfo>,
    /// Control-plane bind address (informational).
    pub ctl_addr: Option<std::net::SocketAddr>,
    /// Phantom-proxy bind address (informational).
    pub proxy_addr: Option<std::net::SocketAddr>,
    /// Hostname-routed gateway bind address, when enabled.
    pub gateway_addr: Option<std::net::SocketAddr>,
    /// Snapshot GC policy. `None` when GC is disabled.
    pub snapshot_gc: Option<SnapshotGcConfig>,
    /// Idle-reaper sweeper tick. `0` when disabled.
    pub idle_check_interval_secs: u64,
}

impl ControlPlaneConfig {
    /// Surface obvious misconfiguration up-front rather than at first
    /// request. Called automatically by every `ControlPlane::with_*`
    /// constructor.
    pub fn validate(&self) -> Result<(), CtlError> {
        if self.proxy_base_url.is_empty() {
            return Err(CtlError::Config("proxy_base_url must not be empty".into()));
        }
        if !(self.proxy_base_url.starts_with("http://")
            || self.proxy_base_url.starts_with("https://"))
        {
            return Err(CtlError::Config(
                "proxy_base_url must start with http:// or https://".into(),
            ));
        }
        if self.proxy_base_url.ends_with('/') {
            return Err(CtlError::Config(
                "proxy_base_url must not end with `/`".into(),
            ));
        }
        Ok(())
    }
}

/// Resolve a `state_dir` to an absolute path, creating the `workspaces/`
/// and `snapshots/` subdirs if missing.
fn ensure_state_dir(state_dir: Option<PathBuf>) -> PathBuf {
    let root = state_dir.unwrap_or_else(|| std::env::temp_dir().join("agentjail-state"));
    for sub in ["workspaces", "snapshots"] {
        let p = root.join(sub);
        if let Err(e) = std::fs::create_dir_all(&p) {
            tracing::warn!(path = %p.display(), error = %e, "failed to create state subdir");
        }
    }
    root
}

/// Opaque wrapper for a configured Postgres pool. Passed to
/// [`ControlPlane::with_postgres`] to swap the credential/audit/jail stores
/// to DB-backed implementations.
pub struct Postgres {
    /// Underlying pool. Public so callers can build their own store
    /// implementations (e.g. the server's bespoke audit sink).
    pub pool: sqlx::PgPool,
}

impl Postgres {
    /// Connect + run the embedded migrations.
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = db::connect(database_url).await?;
        Ok(Self { pool })
    }

    /// Populate the given in-memory `KeyStore` from persisted credentials.
    pub async fn rehydrate_keys(
        &self,
        keys: &Arc<InMemoryKeyStore>,
    ) -> Result<usize, sqlx::Error> {
        db::rehydrate_keystore(&self.pool, keys).await
    }
}

/// Assembled control plane. Call [`Self::router`] for the axum router.
pub struct ControlPlane {
    state: AppState,
    api_keys: ApiKeys,
}

impl ControlPlane {
    /// Build from a config. Uses in-memory stores for sessions, credentials,
    /// and audit. Use [`Self::with_stores`] if you need to swap in durable
    /// implementations.
    #[must_use]
    pub fn new(config: ControlPlaneConfig) -> Self {
        Self::with_all_stores(
            config,
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryCredentialStore::new()),
            Arc::new(InMemoryAuditStore::new()),
            Arc::new(InMemoryJailStore::new()),
            Arc::new(InMemoryWorkspaceStore::new()),
            Arc::new(InMemorySnapshotStore::new()),
        )
    }

    /// Build with explicit stores *including* the jail ledger + workspace
    /// registry + snapshot store — the most granular constructor. Used by
    /// the Postgres-backed wiring.
    #[must_use]
    pub fn with_all_stores(
        config: ControlPlaneConfig,
        sessions: Arc<dyn SessionStore>,
        credentials: Arc<dyn CredentialStore>,
        audit: Arc<dyn AuditStore>,
        jails: Arc<dyn JailStore>,
        workspaces: Arc<dyn WorkspaceStore>,
        snapshots: Arc<dyn SnapshotStore>,
    ) -> Self {
        let proxy_base_url = config.proxy_base_url.trim_end_matches('/').to_string();
        let max_concurrent = config.exec.as_ref().map(|e| e.max_concurrent).unwrap_or(16);
        let exec_semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let exec_metrics = Arc::new(ExecMetrics::new());
        let state_dir = ensure_state_dir(config.state_dir);
        let snapshot_pool_dir = config.snapshot_pool_dir.inspect(|p| {
            if let Err(e) = std::fs::create_dir_all(p) {
                tracing::warn!(path = %p.display(), error = %e,
                    "failed to create snapshot pool dir");
            }
        });
        let workspace_locks  = Arc::new(WorkspaceLocks::new());
        let active_cgroups   = Arc::new(ActiveCgroups::new());
        // Flavors live under `$state_dir/flavors/` by default. The
        // directory may not exist — the scanner treats absence as "no
        // flavors" so fresh installs boot cleanly; operators drop
        // runtime layers in later without restarting.
        let flavors: Arc<dyn FlavorRegistry> = Arc::new(
            crate::flavors::DirFlavorRegistry::new(state_dir.join("flavors"))
        );
        // Share with the gateway when the caller passed an Arc in —
        // otherwise own a private one. Either way the exec path
        // publishes to the same registry AppState sees.
        let active_jail_ips  = config
            .active_jail_ips
            .unwrap_or_else(|| Arc::new(ActiveJailIps::new()));
        let state = AppState {
            tokens: config.tokens,
            keys: config.keys,
            sessions,
            credentials,
            audit,
            proxy_base_url,
            exec_config: config.exec,
            exec_semaphore,
            exec_metrics,
            jails,
            workspaces,
            workspace_locks,
            active_cgroups,
            active_jail_ips,
            snapshots,
            state_dir,
            snapshot_pool_dir,
            platform: config.platform,
            flavors,
        };
        Self {
            state,
            api_keys: ApiKeys::from_config_strings(config.api_keys),
        }
    }

    /// Build with Postgres-backed stores for credentials, audit, jails, and
    /// workspaces. Sessions are now PG-backed too so multi-instance
    /// deploys share state; in-flight tokens survive control-plane
    /// restarts. Call `Postgres::rehydrate_keys` separately before
    /// serving traffic if you want to seed the phantom key store.
    ///
    /// Spawns a background sweeper that deletes expired sessions +
    /// tokens every 60 s. The returned [`ControlPlane`] keeps the
    /// sweeper alive as long as it's alive; drop it to stop.
    #[must_use]
    pub fn with_postgres(config: ControlPlaneConfig, pg: &Postgres) -> Self {
        let sessions:    Arc<dyn SessionStore>    = Arc::new(db::PgSessionStore::new(pg.pool.clone()));
        let credentials: Arc<dyn CredentialStore> = Arc::new(db::PgCredentialStore::new(pg.pool.clone()));
        let audit:       Arc<dyn AuditStore>      = Arc::new(db::PgAuditStore::new(pg.pool.clone()));
        let jails:       Arc<dyn JailStore>       = Arc::new(db::PgJailStore::new(pg.pool.clone()));
        let workspaces:  Arc<dyn WorkspaceStore>  = Arc::new(db::PgWorkspaceStore::new(pg.pool.clone()));
        let snapshots:   Arc<dyn SnapshotStore>   = Arc::new(db::PgSnapshotStore::new(pg.pool.clone()));

        // Sweeper: best-effort, fire-and-forget. Failures log; no
        // retry loop — the next tick handles it. Leaks the handle on
        // purpose — the control-plane is meant to own it for process
        // lifetime.
        let pool = pg.pool.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let sess = db::sweep_expired_sessions(&pool).await;
                // Session delete cascades into phantom_tokens via FK,
                // but tokens can have a tighter TTL than their session
                // so we sweep them directly too.
                let toks = db::sweep_expired_tokens(&pool).await;
                if sess > 0 || toks > 0 {
                    tracing::info!(
                        sessions_deleted = sess,
                        tokens_deleted = toks,
                        "sessions/tokens sweeper"
                    );
                }
            }
        });

        Self::with_all_stores(config, sessions, credentials, audit, jails, workspaces, snapshots)
    }

    /// Best-effort startup reconciliation. Drops workspace rows whose
    /// on-disk dirs have disappeared (e.g. tmpfs was wiped, or a deleted
    /// workspace's row somehow survived). Call after construction.
    pub async fn reconcile(&self) {
        routes::reconcile_workspaces_on_startup(
            self.state.workspaces.as_ref(),
            &self.state.state_dir,
        )
        .await;
    }

    /// Build the axum router.
    pub fn router(self) -> Router {
        let public = Router::new()
            .route("/healthz", get(routes::healthz))
            .route("/v1/stats", get(routes::stats))
            .with_state(self.state.clone());

        let guarded = Router::new()
            .route(
                "/v1/credentials",
                post(routes::put_credential).get(routes::list_credentials),
            )
            .route(
                "/v1/credentials/:service",
                delete(routes::delete_credential),
            )
            .route(
                "/v1/sessions",
                post(routes::create_session).get(routes::list_sessions),
            )
            .route(
                "/v1/sessions/:id",
                get(routes::get_session).delete(routes::delete_session),
            )
            .route("/v1/sessions/:id/exec", post(routes::exec_in_session))
            .route("/v1/runs", post(routes::create_run))
            .route("/v1/runs/fork", post(routes::create_fork_run))
            .route("/v1/runs/stream", post(routes::create_stream_run))
            .route("/v1/audit", get(routes::list_audit))
            .route("/v1/config", get(routes::get_settings))
            .route("/v1/whoami", get(routes::whoami))
            .route("/v1/flavors", get(routes::list_flavors))
            .route("/v1/jails", get(routes::list_jails))
            .route("/v1/jails/:id", get(routes::get_jail))
            .route(
                "/v1/workspaces",
                post(routes::create_workspace).get(routes::list_workspaces),
            )
            .route(
                "/v1/workspaces/:id",
                get(routes::get_workspace)
                    .patch(routes::patch_workspace)
                    .delete(routes::delete_workspace),
            )
            .route(
                "/v1/workspaces/:id/exec",
                post(routes::exec_in_workspace),
            )
            .route(
                "/v1/workspaces/:id/fork",
                post(routes::fork_workspace),
            )
            .route(
                "/v1/workspaces/:id/snapshot",
                post(routes::create_snapshot),
            )
            .route(
                "/v1/workspaces/from-snapshot",
                post(routes::create_workspace_from_snapshot),
            )
            .route(
                "/v1/snapshots",
                get(routes::list_snapshots),
            )
            .route(
                "/v1/snapshots/:id",
                get(routes::get_snapshot).delete(routes::delete_snapshot),
            )
            .route(
                "/v1/snapshots/:id/manifest",
                get(routes::get_snapshot_manifest),
            )
            .layer(RequestBodyLimitLayer::new(1024 * 1024)) // 1 MB
            .layer(middleware::from_fn_with_state(
                self.api_keys.clone(),
                auth::require_api_key,
            ))
            .with_state(self.state.clone());

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        public.merge(guarded)
            .layer(cors)
            .layer(TraceLayer::new_for_http())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentjail_phantom::InMemoryTokenStore;

    fn cfg(url: &str) -> ControlPlaneConfig {
        ControlPlaneConfig {
            tokens: Arc::new(InMemoryTokenStore::new()),
            keys: Arc::new(InMemoryKeyStore::new()),
            proxy_base_url: url.into(),
            api_keys: vec![],
            exec: None,
            state_dir: None,
            snapshot_pool_dir: None,
            platform: None,
            active_jail_ips: None,
        }
    }

    #[test]
    fn validate_accepts_http_url_without_trailing_slash() {
        assert!(cfg("http://localhost:8443").validate().is_ok());
        assert!(cfg("https://proxy.example.com").validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_url() {
        let err = cfg("").validate().unwrap_err();
        assert!(matches!(err, CtlError::Config(_)), "got {err:?}");
    }

    #[test]
    fn validate_rejects_url_without_scheme() {
        assert!(matches!(cfg("localhost:8443").validate(), Err(CtlError::Config(_))));
        assert!(matches!(cfg("//proxy").validate(),         Err(CtlError::Config(_))));
    }

    #[test]
    fn validate_rejects_trailing_slash() {
        assert!(matches!(cfg("http://x/").validate(), Err(CtlError::Config(_))));
    }
}
