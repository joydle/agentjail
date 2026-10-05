//! Postgres-backed [`SessionStore`].
//!
//! Durable sessions — survive control-plane restarts and make
//! multi-instance deploys possible. Schema lives in
//! `0009_sessions_tokens.sql`. Expired rows are cleaned up by
//! [`sweep_expired_sessions`] running on a tick.

use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use crate::error::{CtlError, Result};
use crate::session::{Session, SessionStore};

/// PG-backed session store.
pub struct PgSessionStore {
    pool: PgPool,
}

impl PgSessionStore {
    /// New store over an open pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_session(row: &sqlx::postgres::PgRow) -> Option<Session> {
    let services_json: serde_json::Value = row.try_get("services").ok()?;
    let env_json:      serde_json::Value = row.try_get("env").ok()?;
    let services = serde_json::from_value(services_json).ok()?;
    let env: HashMap<String, String> = serde_json::from_value(env_json).ok()?;
    Some(Session {
        id:         row.try_get::<String, _>("id").ok()?,
        tenant_id:  row.try_get::<String, _>("tenant_id").unwrap_or_else(|_| "dev".into()),
        created_at: row.try_get::<OffsetDateTime, _>("created_at").ok()?,
        expires_at: row.try_get::<Option<OffsetDateTime>, _>("expires_at").ok().flatten(),
        services,
        env,
    })
}

const SESS_COLS: &str = "id, tenant_id, created_at, expires_at, services, env";

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn insert(&self, session: Session) -> Result<()> {
        let services_json = serde_json::to_value(&session.services)
            .map_err(|e| CtlError::Internal(format!("session.services encode: {e}")))?;
        let env_json = serde_json::to_value(&session.env)
            .map_err(|e| CtlError::Internal(format!("session.env encode: {e}")))?;
        let r = sqlx::query(
            "INSERT INTO sessions (id, tenant_id, created_at, expires_at, services, env)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&session.id)
        .bind(&session.tenant_id)
        .bind(session.created_at)
        .bind(session.expires_at)
        .bind(&services_json)
        .bind(&env_json)
        .execute(&self.pool)
        .await;

        match r {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(dbe)) if dbe.code().as_deref() == Some("23505") => {
                Err(CtlError::Conflict(format!("session {} already exists", session.id)))
            }
            Err(e) => Err(CtlError::Internal(format!("session insert: {e}"))),
        }
    }

    async fn get(&self, id: &str) -> Option<Session> {
        let sql = format!(
            "SELECT {SESS_COLS} FROM sessions WHERE id = $1
             AND (expires_at IS NULL OR expires_at > now())"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()?;
        row_to_session(&row)
    }

    async fn list(&self, tenant: Option<&str>) -> Vec<Session> {
        let (sql, rows) = match tenant {
            Some(t) => {
                let sql = format!(
                    "SELECT {SESS_COLS} FROM sessions
                     WHERE tenant_id = $1
                       AND (expires_at IS NULL OR expires_at > now())
                     ORDER BY created_at DESC"
                );
                (
                    sql.clone(),
                    sqlx::query(&sql).bind(t).fetch_all(&self.pool).await.unwrap_or_default(),
                )
            }
            None => {
                let sql = format!(
                    "SELECT {SESS_COLS} FROM sessions
                     WHERE expires_at IS NULL OR expires_at > now()
                     ORDER BY created_at DESC"
                );
                (
                    sql.clone(),
                    sqlx::query(&sql).fetch_all(&self.pool).await.unwrap_or_default(),
                )
            }
        };
        let _ = sql; // silence unused — kept so future tracing can log it
        rows.iter().filter_map(row_to_session).collect()
    }

    async fn remove(&self, id: &str) -> Option<Session> {
        let sql = format!(
            "DELETE FROM sessions WHERE id = $1 RETURNING {SESS_COLS}"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()?;
        row_to_session(&row)
    }
}

/// Delete every session whose `expires_at` has passed. Cascades into
/// `phantom_tokens` via the FK. Returns the deletion count.
pub async fn sweep_expired_sessions(pool: &PgPool) -> u64 {
    sqlx::query(
        "DELETE FROM sessions
         WHERE expires_at IS NOT NULL AND expires_at <= now()"
    )
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0)
}
