//! Tenant scoping primitives — the per-request identity derived from an
//! API key, and the parsing rules that turn config strings into
//! scoped keys.
//!
//! Every resource (workspace, snapshot, session, jail, credential,
//! audit row) carries a `tenant_id`. The auth middleware attaches a
//! [`TenantScope`] to each request, and handlers filter by it so
//! operators in tenant A cannot see tenant B.

use std::str::FromStr;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;

/// Role carried by an API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Sees every tenant's data and may perform any action. Intended
    /// for the platform operator, not for customer-facing keys.
    Admin,
    /// Sees only resources stamped with its own tenant id.
    Operator,
}

impl Role {
    /// Whether this role bypasses per-tenant filtering in store calls.
    #[must_use]
    pub fn is_admin(self) -> bool {
        matches!(self, Role::Admin)
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "admin"    => Ok(Role::Admin),
            "operator" => Ok(Role::Operator),
            other      => Err(format!("role must be `admin` or `operator`, got {other:?}")),
        }
    }
}

/// Identity attached to each authenticated request. Read from request
/// extensions via the [`FromRequestParts`] impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantScope {
    /// Tenant the key belongs to. For admins this is usually `"default"`
    /// or the platform's own tenant — admins aren't filtered on it.
    pub tenant: String,
    /// Role carried by the key.
    pub role: Role,
}

impl TenantScope {
    /// Scope used only when auth is disabled (empty key list) — a mode
    /// the server refuses to start in outside of local dev/tests.
    /// Acts as a platform-admin under the `"dev"` tenant.
    #[must_use]
    pub fn dev_admin() -> Self {
        Self {
            tenant: "dev".into(),
            role: Role::Admin,
        }
    }

    /// Whether this scope can read/write rows belonging to `tenant`.
    /// Admins see every tenant; operators only their own.
    #[must_use]
    pub fn can_see(&self, tenant: &str) -> bool {
        self.role.is_admin() || self.tenant == tenant
    }
}

/// A parsed API key configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyConfig {
    /// The opaque token the caller sends in the `Authorization` header.
    pub token: String,
    /// Tenant this key belongs to.
    pub tenant: String,
    /// Role this key grants.
    pub role: Role,
}

impl ApiKeyConfig {
    /// Scope this key maps to at request time.
    #[must_use]
    pub fn scope(&self) -> TenantScope {
        TenantScope {
            tenant: self.tenant.clone(),
            role: self.role,
        }
    }
}

/// Parse a config-line into a structured key. Required form:
/// `token@tenant:role`. Every component is mandatory — there's no
/// implicit default, so a misconfigured entry fails loud rather than
/// silently granting platform-admin.
///
/// `token` must be non-empty and free of `@`/`:`/whitespace. `tenant`
/// must match `[a-z0-9][a-z0-9_-]{0,63}` so it maps cleanly into DB
/// rows, log fields, and UI badges. `role` is `admin` or `operator`.
pub fn parse_key(s: &str) -> Result<ApiKeyConfig, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty api key".into());
    }
    let (token, rest) = s
        .split_once('@')
        .ok_or_else(|| "api key must be token@tenant:role".to_string())?;
    let (tenant, role_s) = rest
        .split_once(':')
        .ok_or_else(|| "api key must be token@tenant:role".to_string())?;

    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(format!("invalid token: {token:?}"));
    }
    validate_tenant(tenant)?;
    let role: Role = role_s.parse()?;

    Ok(ApiKeyConfig {
        token: token.to_string(),
        tenant: tenant.to_string(),
        role,
    })
}

fn validate_tenant(t: &str) -> Result<(), String> {
    if t.is_empty() || t.len() > 64 {
        return Err(format!("tenant length must be 1..=64: {t:?}"));
    }
    let mut chars = t.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(format!("tenant must start [a-z0-9]: {t:?}"));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(format!("tenant chars must match [a-z0-9_-]: {t:?}"));
        }
    }
    Ok(())
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for TenantScope {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<TenantScope>()
            .cloned()
            .ok_or((
                StatusCode::INTERNAL_SERVER_ERROR,
                "tenant scope missing — auth middleware not wired",
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_form() {
        let k = parse_key("ak_abc@acme:operator").unwrap();
        assert_eq!(k.token, "ak_abc");
        assert_eq!(k.tenant, "acme");
        assert_eq!(k.role, Role::Operator);

        let k = parse_key("ak_ops@platform:admin").unwrap();
        assert_eq!(k.role, Role::Admin);
    }

    #[test]
    fn rejects_partial_forms() {
        // Every component is mandatory — no silent "admin/default" grants.
        assert!(parse_key("ak_abc").is_err());
        assert!(parse_key("ak_abc@acme").is_err());
        assert!(parse_key("").is_err());
    }

    #[test]
    fn rejects_bad_role_and_tenant() {
        assert!(parse_key("ak_abc@acme:god").is_err());
        assert!(parse_key("ak_abc@ACME:admin").is_err()); // uppercase
        assert!(parse_key("ak_abc@-bad:admin").is_err()); // leading dash
        assert!(parse_key("ak_abc@ acme:admin").is_err());// space
    }

    #[test]
    fn rejects_whitespace_in_token() {
        assert!(parse_key("ak abc@acme:admin").is_err());
    }

    #[test]
    fn scope_admin_sees_any_tenant() {
        let s = TenantScope { tenant: "platform".into(), role: Role::Admin };
        assert!(s.can_see("platform"));
        assert!(s.can_see("acme"));
    }

    #[test]
    fn scope_operator_scoped_to_own_tenant() {
        let s = TenantScope { tenant: "acme".into(), role: Role::Operator };
        assert!(s.can_see("acme"));
        assert!(!s.can_see("other"));
    }
}
