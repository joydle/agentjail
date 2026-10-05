//! Postgres-backed implementation of `JailStore`.

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use crate::jails::{
    JailConfigSnapshot, JailKind, JailQuery, JailRecord, JailStatus, JailStore, OUTPUT_CAP,
    truncate,
};

/// Postgres-backed jail ledger.
pub struct PgJailStore {
    pool: PgPool,
}

impl PgJailStore {
    /// Construct from an open pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_record(row: &sqlx::postgres::PgRow) -> JailRecord {
    JailRecord {
        id:                row.get::<i64, _>("id"),
        tenant_id:         row
            .try_get::<String, _>("tenant_id")
            .unwrap_or_else(|_| "dev".to_string()),
        kind:              JailKind::from_str_or_run(row.get::<&str, _>("kind")),
        started_at:        row.get::<OffsetDateTime, _>("started_at"),
        ended_at:          row.get::<Option<OffsetDateTime>, _>("ended_at"),
        status:            JailStatus::from_str_or_error(row.get::<&str, _>("status")),
        session_id:        row.get::<Option<String>, _>("session_id"),
        label:             row.get::<String, _>("label"),
        exit_code:         row.get::<Option<i32>, _>("exit_code"),
        duration_ms:       row.get::<Option<i64>, _>("duration_ms").map(|v| v as u64),
        timed_out:         row.get::<Option<bool>, _>("timed_out"),
        oom_killed:        row.get::<Option<bool>, _>("oom_killed"),
        memory_peak_bytes: row.get::<Option<i64>, _>("memory_peak_bytes").map(|v| v as u64),
        memory_current_bytes: row
            .try_get::<Option<i64>, _>("memory_current_bytes")
            .ok()
            .flatten()
            .map(|v| v as u64),
        cpu_usage_usec:    row.get::<Option<i64>, _>("cpu_usage_usec").map(|v| v as u64),
        pids_current: row
            .try_get::<Option<i64>, _>("pids_current")
            .ok()
            .flatten()
            .map(|v| v as u64),
        io_read_bytes:     row.get::<Option<i64>, _>("io_read_bytes").map(|v| v as u64),
        io_write_bytes:    row.get::<Option<i64>, _>("io_write_bytes").map(|v| v as u64),
        stdout:            row.get::<Option<String>, _>("stdout"),
        stderr:            row.get::<Option<String>, _>("stderr"),
        error:             row.get::<Option<String>, _>("error"),
        parent_id:         row.get::<Option<i64>, _>("parent_id"),
        config:            row
            .try_get::<Option<serde_json::Value>, _>("config_json")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_value::<JailConfigSnapshot>(v).ok()),
    }
}

#[async_trait]
impl JailStore for PgJailStore {
    async fn start(
        &self,
        tenant_id: String,
        kind: JailKind,
        label: String,
        session_id: Option<String>,
        parent_id: Option<i64>,
    ) -> i64 {
        // Trait contract: returns the new row id or a negative sentinel
        // on DB failure. Callers pass the sentinel back to `finish` /
        // `error` / `tail`, which are all `WHERE id = $1` updates and
        // therefore no-op on a row that doesn't exist — so a failed
        // ledger write never aborts an in-flight exec. We still log
        // loudly so operators see the failure.
        match sqlx::query_scalar::<_, i64>(
            "INSERT INTO jails (tenant_id, kind, started_at, status, label, session_id, parent_id)
             VALUES ($1, $2, now(), 'running', $3, $4, $5)
             RETURNING id",
        )
        .bind(&tenant_id)
        .bind(kind.as_str())
        .bind(&label)
        .bind(&session_id)
        .bind(parent_id)
        .fetch_one(&self.pool)
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    tenant_id = %tenant_id,
                    kind = kind.as_str(),
                    label = %label,
                    session_id = session_id.as_deref().unwrap_or(""),
                    parent_id,
                    "jail ledger INSERT failed — subsequent finish/error/tail calls will no-op"
                );
                i64::MIN
            }
        }
    }

    async fn finish(&self, id: i64, output: &agentjail::Output) {
        let duration_ms = i64::try_from(output.duration.as_millis()).unwrap_or(i64::MAX);
        let (mem_peak, mem_cur, cpu, ior, iow, pids) = match output.stats.as_ref() {
            Some(s) => (
                Some(s.memory_peak_bytes as i64),
                Some(s.memory_current_bytes as i64),
                Some(s.cpu_usage_usec as i64),
                Some(s.io_read_bytes as i64),
                Some(s.io_write_bytes as i64),
                Some(s.pids_current as i64),
            ),
            None => (None, None, None, None, None, None),
        };
        let stdout = truncate(&String::from_utf8_lossy(&output.stdout), OUTPUT_CAP);
        let stderr = truncate(&String::from_utf8_lossy(&output.stderr), OUTPUT_CAP);

        let _ = sqlx::query(
            "UPDATE jails SET
                ended_at = now(), status = 'completed',
                exit_code = $2, duration_ms = $3,
                timed_out = $4, oom_killed = $5,
                memory_peak_bytes = $6, memory_current_bytes = $7,
                cpu_usage_usec = $8,
                io_read_bytes = $9, io_write_bytes = $10,
                pids_current = $11,
                stdout = $12, stderr = $13
             WHERE id = $1",
        )
        .bind(id)
        .bind(output.exit_code)
        .bind(duration_ms)
        .bind(output.timed_out)
        .bind(output.oom_killed)
        .bind(mem_peak).bind(mem_cur)
        .bind(cpu)
        .bind(ior).bind(iow)
        .bind(pids)
        .bind(stdout).bind(stderr)
        .execute(&self.pool)
        .await;
    }

    async fn sample_stats(&self, id: i64, stats: &agentjail::ResourceStats) {
        let _ = sqlx::query(
            "UPDATE jails SET
                memory_peak_bytes    = $2,
                memory_current_bytes = $3,
                cpu_usage_usec       = $4,
                io_read_bytes        = $5,
                io_write_bytes       = $6,
                pids_current         = $7
             WHERE id = $1 AND status = 'running'",
        )
        .bind(id)
        .bind(stats.memory_peak_bytes as i64)
        .bind(stats.memory_current_bytes as i64)
        .bind(stats.cpu_usage_usec as i64)
        .bind(stats.io_read_bytes as i64)
        .bind(stats.io_write_bytes as i64)
        .bind(stats.pids_current as i64)
        .execute(&self.pool)
        .await;
    }

    async fn error(&self, id: i64, message: String) {
        let _ = sqlx::query(
            "UPDATE jails SET ended_at = now(), status = 'error', error = $2 WHERE id = $1",
        )
        .bind(id)
        .bind(message)
        .execute(&self.pool)
        .await;
    }

    async fn attach_config(&self, id: i64, config: JailConfigSnapshot) {
        let json = match serde_json::to_value(&config) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, %id, "attach_config: serialize failed");
                return;
            }
        };
        let _ = sqlx::query("UPDATE jails SET config_json = $2 WHERE id = $1")
            .bind(id)
            .bind(json)
            .execute(&self.pool)
            .await;
    }

    async fn recent(&self, limit: usize, status: Option<JailStatus>)
        -> (Vec<JailRecord>, u64)
    {
        let limit = limit.min(1000) as i64;
        let rows = if let Some(s) = status {
            sqlx::query(
                "SELECT * FROM jails WHERE status = $1
                 ORDER BY id DESC LIMIT $2",
            )
            .bind(s.as_str())
            .bind(limit)
            .fetch_all(&self.pool).await.unwrap_or_default()
        } else {
            sqlx::query("SELECT * FROM jails ORDER BY id DESC LIMIT $1")
                .bind(limit)
                .fetch_all(&self.pool).await.unwrap_or_default()
        };

        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jails")
            .fetch_one(&self.pool).await.unwrap_or(0);

        (rows.iter().map(row_to_record).collect(), total as u64)
    }

    async fn page(&self, q: JailQuery) -> (Vec<JailRecord>, u64) {
        // Compose a dynamic WHERE clause with positional binds.
        let mut sql = String::from("SELECT * FROM jails WHERE 1=1");
        let mut count_sql = String::from("SELECT COUNT(*) FROM jails WHERE 1=1");
        let mut args: Vec<String> = Vec::new();
        let mut idx: i32 = 0;

        if let Some(t) = q.tenant.as_deref() {
            idx += 1;
            sql.push_str(&format!(" AND tenant_id = ${idx}"));
            count_sql.push_str(&format!(" AND tenant_id = ${idx}"));
            args.push(t.to_string());
        }
        if let Some(s) = q.status {
            idx += 1;
            sql.push_str(&format!(" AND status = ${idx}"));
            count_sql.push_str(&format!(" AND status = ${idx}"));
            args.push(s.as_str().to_string());
        }
        if let Some(k) = q.kind {
            idx += 1;
            sql.push_str(&format!(" AND kind = ${idx}"));
            count_sql.push_str(&format!(" AND kind = ${idx}"));
            args.push(k.as_str().to_string());
        }
        if let Some(n) = q.q.as_ref().filter(|s| !s.trim().is_empty()) {
            idx += 1;
            let like_idx = idx;
            sql.push_str(&format!(
                " AND (label ILIKE ${like_idx} OR session_id ILIKE ${like_idx} OR error ILIKE ${like_idx})"
            ));
            count_sql.push_str(&format!(
                " AND (label ILIKE ${like_idx} OR session_id ILIKE ${like_idx} OR error ILIKE ${like_idx})"
            ));
            args.push(format!("%{}%", n.replace('%', "\\%")));
        }

        let limit  = q.limit.clamp(1, 500) as i64;
        let offset = q.offset as i64;
        sql.push_str(&format!(" ORDER BY id DESC LIMIT {limit} OFFSET {offset}"));

        // Bind args to both queries.
        let mut row_q = sqlx::query(&sql);
        let mut cnt_q = sqlx::query_scalar::<_, i64>(&count_sql);
        for a in &args {
            row_q = row_q.bind(a);
            cnt_q = cnt_q.bind(a);
        }

        let rows = row_q.fetch_all(&self.pool).await.unwrap_or_default();
        let total = cnt_q.fetch_one(&self.pool).await.unwrap_or(0);
        (rows.iter().map(row_to_record).collect(), total as u64)
    }

    async fn tail(&self, id: i64, stdout: &str, stderr: &str) {
        let _ = sqlx::query(
            "UPDATE jails SET stdout = $2, stderr = $3
             WHERE id = $1 AND status = 'running'",
        )
        .bind(id)
        .bind(truncate(stdout, OUTPUT_CAP))
        .bind(truncate(stderr, OUTPUT_CAP))
        .execute(&self.pool)
        .await;
    }

    async fn get(&self, id: i64) -> Option<JailRecord> {
        sqlx::query("SELECT * FROM jails WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool).await
            .ok().flatten()
            .as_ref()
            .map(row_to_record)
    }
}
