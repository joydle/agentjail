//! Credential CRUD — stores provider API keys the phantom proxy resolves.

use agentjail_phantom::{SecretString, ServiceId};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use time::OffsetDateTime;

/// Common `?tenant=<id>` query param — admins use it to address a
/// different tenant; operators ignore it (or get 404 if it targets
/// another tenant, via `target_tenant`).
#[derive(Debug, Deserialize)]
pub(crate) struct TenantQuery {
    #[serde(default)]
    tenant: Option<String>,
}

use crate::credential::{CredentialRecord, fingerprint};
use crate::error::{CtlError, Result};
use crate::tenant::TenantScope;

use super::AppState;

/// Resolve the target tenant for a credential route.
///
/// - Operators can only act on their own tenant; `tenant` query param
///   is ignored (and if present, must match their own scope or we 404
///   to hide that other tenants exist).
/// - Admins default to their own tenant but may act on any tenant by
///   passing `?tenant=<id>` — lets a platform admin seed a customer's
///   credentials during onboarding.
fn target_tenant(scope: &TenantScope, requested: Option<&str>) -> Result<String> {
    match requested {
        None => Ok(scope.tenant.clone()),
        Some(t) if scope.role.is_admin() => Ok(t.to_string()),
        Some(t) if t == scope.tenant => Ok(t.to_string()),
        Some(_) => Err(CtlError::NotFound("not found".into())),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PutCredentialRequest {
    service: ServiceId,
    secret: String,
}

pub(crate) async fn put_credential(
    State(state): State<AppState>,
    scope: TenantScope,
    Query(q): Query<TenantQuery>,
    Json(req): Json<PutCredentialRequest>,
) -> Result<Json<CredentialRecord>> {
    let tenant = target_tenant(&scope, q.tenant.as_deref())?;
    if req.secret.trim().is_empty() {
        return Err(CtlError::BadRequest("secret must be non-empty".into()));
    }
    let fp = fingerprint(&req.secret);
    let now = OffsetDateTime::now_utc();
    let existing = state.credentials.get(&tenant, req.service).await;
    let rec = CredentialRecord {
        tenant_id: tenant.clone(),
        service: req.service,
        added_at: existing.as_ref().map_or(now, |r| r.added_at),
        updated_at: now,
        fingerprint: fp,
    };
    // Persist first (DB is the source of truth when enabled), then update
    // the in-memory key the phantom proxy reads on the hot path.
    state.credentials.upsert_with_secret(rec.clone(), &req.secret).await;
    state.keys.set(tenant.clone(), req.service, SecretString::new(req.secret));
    Ok(Json(rec))
}

pub(crate) async fn list_credentials(
    State(state): State<AppState>,
    scope: TenantScope,
    Query(q): Query<TenantQuery>,
) -> Result<Json<Vec<CredentialRecord>>> {
    // Operators only see their own tenant; admins see every tenant
    // unless they scope down with `?tenant=<id>`.
    let filter: Option<String> = if scope.role.is_admin() {
        q.tenant.clone()
    } else {
        Some(scope.tenant.clone())
    };
    Ok(Json(state.credentials.list(filter.as_deref()).await))
}

pub(crate) async fn delete_credential(
    State(state): State<AppState>,
    scope: TenantScope,
    Query(q): Query<TenantQuery>,
    Path(service): Path<String>,
) -> Result<StatusCode> {
    let tenant = target_tenant(&scope, q.tenant.as_deref())?;
    let svc = parse_service(&service)?;
    match state.credentials.remove(&tenant, svc).await {
        Some(_) => {
            state.keys.unset(&tenant, svc);
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err(CtlError::NotFound(format!("credential {service}"))),
    }
}

fn parse_service(s: &str) -> Result<ServiceId> {
    match s {
        "openai" => Ok(ServiceId::OpenAi),
        "anthropic" => Ok(ServiceId::Anthropic),
        "github" => Ok(ServiceId::GitHub),
        "stripe" => Ok(ServiceId::Stripe),
        other => Err(CtlError::BadRequest(format!("unknown service {other}"))),
    }
}
