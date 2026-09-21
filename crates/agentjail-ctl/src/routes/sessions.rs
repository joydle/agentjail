//! Session lifecycle — issues phantom tokens per service.

use std::collections::HashMap;
use std::time::Duration;

use agentjail_phantom::{KeyStore, PathGlob, Scope, ServiceId};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{CtlError, Result};
use crate::session::{Session, new_session_id};
use crate::tenant::TenantScope;

use super::AppState;

fn tenant_filter(scope: &TenantScope) -> Option<String> {
    if scope.role.is_admin() { None } else { Some(scope.tenant.clone()) }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateSessionRequest {
    #[serde(default)]
    services: Vec<ServiceId>,
    /// Optional TTL in seconds.
    #[serde(default)]
    ttl_secs: Option<u64>,
    /// Optional per-service allow-list of path globs. Keys must be a subset
    /// of `services`. Missing entries mean unrestricted scope.
    ///
    /// Example:
    /// ```json
    /// { "services": ["openai", "github"],
    ///   "scopes":   { "github": ["/repos/my-org/*/issues*"] } }
    /// ```
    #[serde(default)]
    scopes: HashMap<ServiceId, Vec<String>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreateSessionResponse {
    #[serde(flatten)]
    session: SessionView,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct SessionView {
    id: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    services: Vec<ServiceId>,
    env: HashMap<String, String>,
}

impl From<Session> for SessionView {
    fn from(s: Session) -> Self {
        Self {
            id: s.id,
            created_at: s.created_at,
            expires_at: s.expires_at,
            services: s.services,
            env: s.env,
        }
    }
}

pub(crate) async fn create_session(
    State(state): State<AppState>,
    scope: TenantScope,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<CreateSessionResponse>)> {
    if req.services.is_empty() {
        return Err(CtlError::BadRequest(
            "services must contain at least one entry".into(),
        ));
    }
    // Verify every requested service has a real key configured for
    // the caller's tenant. Admins fall back to the `"dev"` pool if
    // their own tenant hasn't set one — the same sentinel the key
    // store fills from env vars — so a platform-admin using the dev
    // harness still finds keys without jumping through hoops.
    for svc in &req.services {
        let has = state.keys.get(&scope.tenant, *svc).await.is_some()
            || (scope.role.is_admin()
                && state.keys.get("dev", *svc).await.is_some());
        if !has {
            return Err(CtlError::BadRequest(format!(
                "no credential configured for service {svc} (tenant {})",
                scope.tenant,
            )));
        }
    }

    let id = new_session_id();
    let ttl = req.ttl_secs.map(Duration::from_secs);
    let expires_at = ttl.map(|d| OffsetDateTime::now_utc() + d);

    // Validate scopes: every key must be in services.
    for svc in req.scopes.keys() {
        if !req.services.contains(svc) {
            return Err(CtlError::BadRequest(format!(
                "scope for service {svc} but it is not in services"
            )));
        }
    }

    let mut env = HashMap::new();

    // Sessions inherit the caller's tenant. Admins issuing a session
    // don't get to impersonate a different tenant here — they'd use
    // the admin-scoped credential routes for cross-tenant work.
    let session_tenant = scope.tenant.clone();

    for svc in &req.services {
        let path_scope = req
            .scopes
            .get(svc)
            .map(|paths| Scope {
                allowed_paths: paths.iter().map(|p| PathGlob::new(p.clone())).collect(),
            })
            .unwrap_or_else(Scope::any);
        let token = state.tokens.issue(
            id.clone(),
            // Token carries the session's tenant so the phantom
            // proxy can look up the right (tenant, service) pair
            // at forward time.
            session_tenant.clone(),
            *svc,
            path_scope,
            ttl,
        ).await;
        let token_str = token.to_string();
        match svc {
            ServiceId::OpenAi => {
                env.insert("OPENAI_API_KEY".into(), token_str);
                env.insert(
                    "OPENAI_BASE_URL".into(),
                    format!("{}/v1/openai/v1", state.proxy_base_url),
                );
            }
            ServiceId::Anthropic => {
                env.insert("ANTHROPIC_API_KEY".into(), token_str);
                env.insert(
                    "ANTHROPIC_BASE_URL".into(),
                    format!("{}/v1/anthropic", state.proxy_base_url),
                );
            }
            ServiceId::GitHub => {
                env.insert("GITHUB_TOKEN".into(), token_str);
                env.insert(
                    "GITHUB_API_URL".into(),
                    format!("{}/v1/github", state.proxy_base_url),
                );
            }
            ServiceId::Stripe => {
                env.insert("STRIPE_API_KEY".into(), token_str);
                env.insert(
                    "STRIPE_API_BASE".into(),
                    format!("{}/v1/stripe", state.proxy_base_url),
                );
            }
        }
    }

    let session = Session {
        id: id.clone(),
        tenant_id: scope.tenant.clone(),
        created_at: OffsetDateTime::now_utc(),
        expires_at,
        services: req.services,
        env: env.clone(),
    };
    state.sessions.insert(session.clone()).await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateSessionResponse {
            session: session.into(),
        }),
    ))
}

pub(crate) async fn list_sessions(
    State(state): State<AppState>,
    scope: TenantScope,
) -> Json<Vec<SessionView>> {
    let t = tenant_filter(&scope);
    Json(
        state
            .sessions
            .list(t.as_deref())
            .await
            .into_iter()
            .map(SessionView::from)
            .collect(),
    )
}

pub(crate) async fn get_session(
    State(state): State<AppState>,
    scope: TenantScope,
    Path(id): Path<String>,
) -> Result<Json<SessionView>> {
    state
        .sessions
        .get(&id)
        .await
        .filter(|s| scope.can_see(&s.tenant_id))
        .map(SessionView::from)
        .map(Json)
        .ok_or_else(|| CtlError::NotFound(format!("session {id}")))
}

pub(crate) async fn delete_session(
    State(state): State<AppState>,
    scope: TenantScope,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Pre-check ownership; delete-then-discover would leak existence.
    let existing = state.sessions.get(&id).await;
    if !existing.as_ref().is_some_and(|s| scope.can_see(&s.tenant_id)) {
        return Err(CtlError::NotFound(format!("session {id}")));
    }
    let Some(session) = state.sessions.remove(&id).await else {
        return Err(CtlError::NotFound(format!("session {id}")));
    };
    // Revoke every phantom token we issued for this session.
    state.tokens.revoke_session(&session.id).await;
    Ok(StatusCode::NO_CONTENT)
}
