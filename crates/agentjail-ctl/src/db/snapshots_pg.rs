//! Postgres-backed `SnapshotStore`.

use std::path::PathBuf;

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use crate::error::{CtlError, Result};
use crate::snapshots::{SnapshotRecord, SnapshotStore};

/// PG-backed snapshot store.
pub struct PgSnapshotStore {
    pool: PgPool,
}

impl PgSnapshotStore {
    /// New store over an open pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_record(row: &sqlx::postgres::PgRow) -> SnapshotRecord {
    SnapshotRecord {
        id:           row.get::<String, _>("id"),
        // `try_get` for back-compat with rows that predate the tenancy
        // migration — they surface under `"dev"`.
        tenant_id:    row
            .try_get::<String, _>("tenant_id")
            .unwrap_or_else(|_| "dev".to_string()),
        workspace_id: row.get::<Option<String>, _>("workspace_id"),
        name:         row.get::<Option<String>, _>("name"),
        created_at:   row.get::<OffsetDateTime, _>("created_at"),
        path:         PathBuf::from(row.get::<String, _>("path")),
        size_bytes:   row.get::<i64, _>("size_bytes") as u64,
    }
}

const SNAPSHOT_COLS: &str =
    "id, tenant_id, workspace_id, name, created_at, path, size_bytes";

#[async_trait]
impl SnapshotStore for PgSnapshotStore {
    async fn insert(&self, snap: SnapshotRecord) -> Result<()> {
        let r = sqlx::query(
            "INSERT INTO snapshots (id, tenant_id, workspace_id, name, created_at, path, size_bytes)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&snap.id)
        .bind(&snap.tenant_id)
        .bind(snap.workspace_id.as_deref())
        .bind(snap.name.as_deref())
        .bind(snap.created_at)
        .bind(snap.path.to_string_lossy().as_ref())
        .bind(snap.size_bytes as i64)
        .execute(&self.pool)
        .await;

        match r {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(dbe)) if dbe.code().as_deref() == Some("23505") => {
                Err(CtlError::Conflict(format!("snapshot {} already exists", snap.id)))
            }
            Err(e) => Err(CtlError::Internal(format!("snapshot insert: {e}"))),
        }
    }

    async fn get(&self, id: &str) -> Option<SnapshotRecord> {
        let sql = format!("SELECT {SNAPSHOT_COLS} FROM snapshots WHERE id = $1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()?;
        Some(row_to_record(&row))
    }

    async fn list(
        &self,
        tenant: Option<&str>,
        workspace_id: Option<&str>,
        limit: usize,
        offset: usize,
        q: Option<&str>,
    ) -> (Vec<SnapshotRecord>, u64) {
        let limit_i  = limit.clamp(1, 500) as i64;
        let offset_i = offset as i64;
        let needle   = q.map(str::trim).filter(|s| !s.is_empty());

        // Build WHERE clause + bind list dynamically so each combination
        // of filters produces exactly one SQL query.
        let mut where_sql = String::from("1 = 1");
        let mut args: Vec<String> = Vec::new();
        let mut idx: i32 = 0;

        if let Some(t) = tenant {
            idx += 1;
            where_sql.push_str(&format!(" AND tenant_id = ${idx}"));
            args.push(t.to_string());
        }
        if let Some(w) = workspace_id {
            idx += 1;
            where_sql.push_str(&format!(" AND workspace_id = ${idx}"));
            args.push(w.to_string());
        }
        if let Some(n) = needle {
            idx += 1;
            let at = idx;
            where_sql.push_str(&format!(
                " AND (id ILIKE ${at} ESCAPE '\\' OR name ILIKE ${at} ESCAPE '\\' OR workspace_id ILIKE ${at} ESCAPE '\\')"
            ));
            args.push(format!(
                "%{}%",
                n.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"),
            ));
        }

        let count_sql = format!("SELECT COUNT(*) FROM snapshots WHERE {where_sql}");
        let rows_sql  = format!(
            "SELECT {SNAPSHOT_COLS}
             FROM snapshots
             WHERE {where_sql}
             ORDER BY created_at DESC
             LIMIT ${lim} OFFSET ${off}",
            lim = idx + 1,
            off = idx + 2,
        );

        let mut total_q = sqlx::query_scalar::<_, i64>(&count_sql);
        for a in &args { total_q = total_q.bind(a); }
        let total = total_q.fetch_one(&self.pool).await.unwrap_or(0);

        let mut rows_q = sqlx::query(&rows_sql);
        for a in &args { rows_q = rows_q.bind(a); }
        let rows = rows_q
            .bind(limit_i)
            .bind(offset_i)
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default();

        let parsed: Vec<SnapshotRecord> = rows.iter().map(row_to_record).collect();
        (parsed, total as u64)
    }

    async fn remove(&self, id: &str) -> Option<SnapshotRecord> {
        let sql = format!(
            "DELETE FROM snapshots WHERE id = $1 RETURNING {SNAPSHOT_COLS}"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()?;
        Some(row_to_record(&row))
    }
}
