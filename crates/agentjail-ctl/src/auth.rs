//! API-key authentication middleware.
//!
//! Stores the list of valid keys alongside their [`TenantScope`], so the
//! middleware can stamp each authenticated request with the scope the
//! key maps to. Handlers read the scope via the `TenantScope`
//! extractor and filter every store call by it.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

use crate::tenant::{ApiKeyConfig, TenantScope, parse_key};

/// Collection of valid control-plane API keys + their scopes. The keys
/// list is small (expected ≪ 1000), and constant-time compare is done
/// per entry on every request — the linear scan is fine.
#[derive(Clone, Default)]
pub struct ApiKeys {
    keys: Arc<Vec<ApiKeyConfig>>,
}

impl ApiKeys {
    /// Build from a list of config strings in `token[@tenant[:role]]`
    /// form. Malformed entries are skipped with a warning so a single
    /// typo doesn't lock the operator out; if *every* line is
    /// malformed and the list wasn't empty to begin with, we return an
    /// empty `ApiKeys` which disables auth — caller should check
    /// `is_enforced` and refuse to start in that case.
    #[must_use]
    pub fn from_config_strings(lines: impl IntoIterator<Item = String>) -> Self {
        let mut parsed = Vec::new();
        for line in lines {
            match parse_key(&line) {
                Ok(k) => parsed.push(k),
                Err(e) => {
                    // Don't log the line itself — it contains the token.
                    tracing::warn!(error = %e, "skipping malformed api key entry");
                }
            }
        }
        Self { keys: Arc::new(parsed) }
    }

    /// Build from already-parsed configs (used by tests + programmatic
    /// embedders who don't need the string form).
    #[must_use]
    pub fn from_configs(configs: impl IntoIterator<Item = ApiKeyConfig>) -> Self {
        Self {
            keys: Arc::new(configs.into_iter().collect()),
        }
    }

    /// Whether any key is configured.
    #[must_use]
    pub fn is_enforced(&self) -> bool {
        !self.keys.is_empty()
    }

    fn match_scope(&self, presented: &str) -> Option<TenantScope> {
        // Constant-time compare against every known token. We walk the
        // full list (don't short-circuit) so an attacker can't learn
        // key-prefix info from response timing.
        let mut winner: Option<TenantScope> = None;
        for k in self.keys.iter() {
            let hit: bool = k.token.as_bytes().ct_eq(presented.as_bytes()).into();
            if hit && winner.is_none() {
                winner = Some(k.scope());
            }
        }
        winner
    }
}

/// Middleware: require `Authorization: Bearer <api-key>` when enforced,
/// and stamp the matched [`TenantScope`] onto the request extensions.
pub async fn require_api_key(
    State(keys): State<ApiKeys>,
    headers: HeaderMap,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    if !keys.is_enforced() {
        // Auth disabled → everything runs as a dev admin. Only useful
        // for local dev and tests; the server refuses to launch in
        // prod without at least one key.
        req.extensions_mut().insert(TenantScope::dev_admin());
        return next.run(req).await;
    }
    let Some(raw) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return (StatusCode::UNAUTHORIZED, "missing api key").into_response();
    };
    let token = raw.strip_prefix("Bearer ").unwrap_or(raw).trim();
    let Some(scope) = keys.match_scope(token) else {
        return (StatusCode::UNAUTHORIZED, "invalid api key").into_response();
    };
    req.extensions_mut().insert(scope);
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenant::Role;

    #[test]
    fn match_scope_finds_token_and_returns_its_scope() {
        let keys = ApiKeys::from_configs(vec![
            ApiKeyConfig { token: "ak_a".into(), tenant: "acme".into(),  role: Role::Operator },
            ApiKeyConfig { token: "ak_b".into(), tenant: "platform".into(), role: Role::Admin },
        ]);
        let s = keys.match_scope("ak_b").unwrap();
        assert_eq!(s.tenant, "platform");
        assert!(s.role.is_admin());
    }

    #[test]
    fn match_scope_returns_none_for_unknown_token() {
        let keys = ApiKeys::from_configs(vec![ApiKeyConfig {
            token: "ak_a".into(), tenant: "acme".into(), role: Role::Operator,
        }]);
        assert!(keys.match_scope("ak_nope").is_none());
    }

    #[test]
    fn from_config_strings_skips_malformed_entries() {
        let keys = ApiKeys::from_config_strings(vec![
            "ak_good@acme:operator".to_string(),
            "ak bad".to_string(),   // space → rejected
            "".to_string(),         // empty → rejected
        ]);
        assert_eq!(keys.keys.len(), 1);
        assert_eq!(keys.keys[0].tenant, "acme");
    }
}
