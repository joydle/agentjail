//! Postgres-backed implementation of `AuditStore`.

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use crate::audit::{AuditRow, AuditStore};

/// Postgres-backed audit log.
pub struct PgAuditStore {
    pool: PgPool,
}

impl PgAuditStore {
    /// New store over an open pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_audit(row: &sqlx::postgres::PgRow) -> AuditRow {
    AuditRow {
        id:            row.get::<i64, _>("id") as u64,
        at:            row.get::<OffsetDateTime, _>("at"),
        session_id:    row.get::<String, _>("session_id"),
        service:       row.get::<String, _>("service"),
        method:        row.get::<String, _>("method"),
        path:          row.get::<String, _>("path"),
        status:        row.get::<i32, _>("status") as u16,
        reject_reason: row.get::<Option<String>, _>("reject_reason"),
        upstream_ms:   row.get::<Option<i64>, _>("upstream_ms").map(|v| v as u64),
    }
}

#[async_trait]
impl AuditStore for PgAuditStore {
    async fn push(&self, row: AuditRow) {
        // `id` from the incoming row is ignored — we use DB's bigserial.
        let _ = sqlx::query(
            "INSERT INTO audit_log
               (at, session_id, service, method, path, status, reject_reason, upstream_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(row.at)
        .bind(row.session_id)
        .bind(row.service)
        .bind(row.method)
        .bind(row.path)
        .bind(row.status as i32)
        .bind(row.reject_reason)
        .bind(row.upstream_ms.map(|v| v as i64))
        .execute(&self.pool)
        .await;
    }

    async fn recent(&self, limit: usize) -> Vec<AuditRow> {
        let limit = limit.min(1000) as i64;
        let rows = sqlx::query(
            "SELECT id, at, session_id, service, method, path, status, reject_reason, upstream_ms
             FROM audit_log ORDER BY id DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().map(row_to_audit).collect()
    }

    async fn total(&self) -> u64 {
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
        total as u64
    }
}
