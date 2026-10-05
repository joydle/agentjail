//! Postgres-backed [`agentjail_phantom::TokenStore`].
//!
//! Wrap this in [`agentjail_phantom::LruTokenCache`] on the hot path —
//! the proxy's per-request `lookup` goes through the cache so a
//! cold-start penalty only hits once per token.

use std::time::{Duration, SystemTime};

use agentjail_phantom::{PhantomToken, Scope, ServiceId, TokenRecord, TokenStore};
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

/// PG-backed phantom-token store.
pub struct PgTokenStore {
    pool: PgPool,
}

impl PgTokenStore {
    /// New store over an open pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn system_time_from(t: OffsetDateTime) -> SystemTime {
    // OffsetDateTime → SystemTime: via unix timestamp, which is
    // monotonic-independent and stable across the epoch boundary
    // our tests care about.
    let dur = Duration::from_nanos(t.unix_timestamp_nanos().max(0) as u64);
    SystemTime::UNIX_EPOCH + dur
}

fn offset_from(t: SystemTime) -> OffsetDateTime {
    let nanos = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn parse_service(s: &str) -> Option<ServiceId> {
    match s {
        "openai"    => Some(ServiceId::OpenAi),
        "anthropic" => Some(ServiceId::Anthropic),
        "github"    => Some(ServiceId::GitHub),
        "stripe"    => Some(ServiceId::Stripe),
        _ => None,
    }
}

fn row_to_record(row: &sqlx::postgres::PgRow) -> Option<TokenRecord> {
    let scope_json: serde_json::Value = row.try_get("scope").ok()?;
    let scope: Scope = serde_json::from_value(scope_json).ok()?;
    Some(TokenRecord {
        session_id: row.try_get::<String, _>("session_id").ok()?,
        tenant_id:  row.try_get::<String, _>("tenant_id").unwrap_or_else(|_| "dev".into()),
        service:    parse_service(row.try_get::<&str, _>("service").ok()?)?,
        scope,
        expires_at: row
            .try_get::<Option<OffsetDateTime>, _>("expires_at")
            .ok()
            .flatten()
            .map(system_time_from),
    })
}

#[async_trait]
impl TokenStore for PgTokenStore {
    async fn issue(
        &self,
        session_id: String,
        tenant_id: String,
        service: ServiceId,
        scope: Scope,
        ttl: Option<Duration>,
    ) -> PhantomToken {
        let token = PhantomToken::generate();
        let expires_at = ttl.map(|d| offset_from(SystemTime::now() + d));
        let scope_json = serde_json::to_value(&scope).unwrap_or(serde_json::Value::Null);
        let _ = sqlx::query(
            "INSERT INTO phantom_tokens
                (token_hash, session_id, tenant_id, service, scope, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (token_hash) DO NOTHING"
        )
        .bind(token.as_bytes().as_slice())
        .bind(&session_id)
        .bind(&tenant_id)
        .bind(service.name())
        .bind(&scope_json)
        .bind(expires_at)
        .execute(&self.pool)
        .await;
        token
    }

    async fn lookup(&self, token: &PhantomToken) -> Option<TokenRecord> {
        let row = sqlx::query(
            "SELECT session_id, tenant_id, service, scope, expires_at
             FROM phantom_tokens
             WHERE token_hash = $1
               AND (expires_at IS NULL OR expires_at > now())"
        )
        .bind(token.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()?;
        row_to_record(&row)
    }

    async fn revoke(&self, token: &PhantomToken) {
        let _ = sqlx::query("DELETE FROM phantom_tokens WHERE token_hash = $1")
            .bind(token.as_bytes().as_slice())
            .execute(&self.pool)
            .await;
    }

    async fn revoke_session(&self, session_id: &str) {
        let _ = sqlx::query("DELETE FROM phantom_tokens WHERE session_id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await;
    }
}

/// Delete every expired phantom-token row. Usually rides on the same
/// tick as `sweep_expired_sessions` — a session's `ON DELETE CASCADE`
/// will sweep its tokens too, but this covers tokens with a tighter
/// TTL than their session.
pub async fn sweep_expired_tokens(pool: &PgPool) -> u64 {
    sqlx::query(
        "DELETE FROM phantom_tokens
         WHERE expires_at IS NOT NULL AND expires_at <= now()"
    )
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0)
}
