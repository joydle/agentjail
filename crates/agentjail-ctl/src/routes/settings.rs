//! `GET /v1/config` — read-only snapshot of the running server's
//! configuration.
//!
//! Exists so operators (and the web Settings page) can see what's
//! actually configured without SSH'ing to the host. All fields here are
//! safe to surface: bind addresses, GC thresholds, provider metadata.
//! Secrets (API keys, database URLs, real upstream credentials) are
//! never included.

use axum::{Json, extract::State};
use serde::Serialize;

use super::AppState;
use crate::ProviderInfo;
use crate::tenant::TenantScope;

/// Reply for `GET /v1/config`.
#[derive(Debug, Serialize)]
pub(crate) struct SettingsResponse {
    proxy:         ProxySettings,
    control_plane: ControlPlaneSettings,
    gateway:       Option<GatewaySettings>,
    exec:          Option<ExecSettings>,
    persistence:   PersistenceSettings,
    snapshots:     SnapshotSettings,
}

#[derive(Debug, Serialize)]
struct ProxySettings {
    /// URL the sandbox points at (`PROXY_BASE_URL`).
    base_url: String,
    /// Phantom-proxy bind address. Redacted for non-admin scopes — the
    /// host-network shape is operator-facing only.
    #[serde(skip_serializing_if = "Option::is_none")]
    bind_addr: Option<String>,
    /// Registered upstream providers (id, upstream base URL, request prefix).
    providers: Vec<ProviderInfo>,
}

#[derive(Debug, Serialize)]
struct ControlPlaneSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    bind_addr: Option<String>,
}

#[derive(Debug, Serialize)]
struct GatewaySettings {
    bind_addr: String,
}

#[derive(Debug, Serialize)]
struct ExecSettings {
    default_memory_mb:   u64,
    default_timeout_secs: u64,
    max_concurrent:      usize,
}

#[derive(Debug, Serialize)]
struct PersistenceSettings {
    /// Absolute host path of the state dir — admin-only. Operators see
    /// this field omitted so recorded dashboards / screenshots don't
    /// leak the daemon's on-disk layout.
    #[serde(skip_serializing_if = "Option::is_none")]
    state_dir:         Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_pool_dir: Option<String>,
    idle_check_secs:   u64,
}

#[derive(Debug, Serialize)]
struct SnapshotSettings {
    /// GC policy. `null` when the GC sweeper is disabled.
    gc: Option<GcPolicy>,
}

#[derive(Debug, Serialize)]
struct GcPolicy {
    max_age_secs: Option<u64>,
    max_count:    Option<usize>,
    tick_secs:    u64,
}

pub(crate) async fn get_settings(
    State(state): State<AppState>,
    scope: TenantScope,
) -> Json<SettingsResponse> {
    let p = state.platform.clone().unwrap_or_default();
    let admin = scope.role.is_admin();
    Json(SettingsResponse {
        proxy: ProxySettings {
            base_url:  state.proxy_base_url.clone(),
            bind_addr: if admin { p.proxy_addr.map(|a| a.to_string()) } else { None },
            providers: p.providers.clone(),
        },
        control_plane: ControlPlaneSettings {
            bind_addr: if admin { p.ctl_addr.map(|a| a.to_string()) } else { None },
        },
        gateway: if admin {
            p.gateway_addr.map(|a| GatewaySettings { bind_addr: a.to_string() })
        } else {
            None
        },
        exec: state.exec_config.as_ref().map(|e| ExecSettings {
            default_memory_mb:    e.default_memory_mb,
            default_timeout_secs: e.default_timeout_secs,
            max_concurrent:       e.max_concurrent,
        }),
        persistence: PersistenceSettings {
            state_dir:         if admin { Some(state.state_dir.display().to_string()) } else { None },
            snapshot_pool_dir: if admin { state.snapshot_pool_dir.as_ref().map(|p| p.display().to_string()) } else { None },
            idle_check_secs:   p.idle_check_interval_secs,
        },
        snapshots: SnapshotSettings {
            gc: p.snapshot_gc.map(|gc| GcPolicy {
                max_age_secs: gc.max_age_secs,
                max_count:    gc.max_count,
                tick_secs:    gc.tick_secs,
            }),
        },
    })
}

// ---------- whoami ----------

/// `GET /v1/whoami` — tells the caller which tenant + role their API
/// key maps to. The UI reads this to decide whether to render
/// admin-only widgets (accounts panel, bind-addrs, allowlist details).
#[derive(Debug, Serialize)]
pub(crate) struct WhoamiResponse {
    tenant: String,
    role:   &'static str,
}

pub(crate) async fn whoami(scope: TenantScope) -> Json<WhoamiResponse> {
    Json(WhoamiResponse {
        tenant: scope.tenant,
        role:   if scope.role.is_admin() { "admin" } else { "operator" },
    })
}

// ---------- flavors ----------

/// `GET /v1/flavors` — list every registered runtime flavor. Any
/// authenticated caller can read this so operators can populate a
/// picker at workspace-create time. The host path is admin-internal
/// and intentionally not exposed here.
#[derive(Debug, Serialize)]
pub(crate) struct FlavorView {
    name: String,
}

pub(crate) async fn list_flavors(
    State(state): State<AppState>,
    _scope: TenantScope,
) -> Json<Vec<FlavorView>> {
    Json(
        state.flavors.list()
            .into_iter()
            .map(|f| FlavorView { name: f.name })
            .collect(),
    )
}

