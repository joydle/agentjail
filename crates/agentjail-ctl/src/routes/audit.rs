//! Audit log read endpoint.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::audit::AuditRow;
use crate::tenant::TenantScope;

use super::AppState;

#[derive(Debug, Deserialize)]
pub(crate) struct AuditQuery {
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AuditListResponse {
    rows: Vec<AuditRow>,
    total: u64,
}

pub(crate) async fn list_audit(
    State(state): State<AppState>,
    scope: TenantScope,
    Query(q): Query<AuditQuery>,
) -> Json<AuditListResponse> {
    // Pull an over-fetch when filtering: audit rows don't carry tenant
    // directly (the phantom AuditEntry doesn't know), so we join against
    // the session ledger post-hoc. If this becomes a hot path we'll
    // extend the sink to stamp tenant at write time; right now audit
    // traffic is low enough that the in-memory filter is fine.
    let limit = q.limit.unwrap_or(100).min(1000);
    let total = state.audit.total().await;

    if scope.role.is_admin() {
        let rows = state.audit.recent(limit).await;
        return Json(AuditListResponse { rows, total });
    }

    // Build the set of session ids owned by this tenant, then keep only
    // audit rows whose session_id matches. Over-fetch by a reasonable
    // multiplier so we usually fill the requested page even after
    // filtering — audit log is a ring buffer so exact totals are
    // already approximate.
    let tenant_sessions: HashSet<String> = state
        .sessions
        .list(Some(scope.tenant.as_str()))
        .await
        .into_iter()
        .map(|s| s.id)
        .collect();

    let raw = state.audit.recent(limit.saturating_mul(4).min(1000)).await;
    let rows: Vec<AuditRow> = raw
        .into_iter()
        .filter(|r| tenant_sessions.contains(&r.session_id))
        .take(limit)
        .collect();

    Json(AuditListResponse { rows, total })
}
