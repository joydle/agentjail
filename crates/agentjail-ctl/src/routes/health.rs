//! Health + public stats.

use axum::Json;
use axum::extract::State;

use super::AppState;

pub(crate) async fn healthz() -> &'static str {
    "ok"
}

/// `GET /v1/stats` — live metrics (public, no auth).
pub(crate) async fn stats(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "active_execs": state.exec_metrics.active(),
        "total_execs": state.exec_metrics.total(),
        // `/v1/stats` is unauthenticated; the counts roll up every
        // tenant's rows, same as before tenancy landed.
        "sessions": state.sessions.list(None).await.len(),
        "credentials": state.credentials.list(None).await.len(),
    }))
}
