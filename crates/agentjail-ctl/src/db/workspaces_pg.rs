//! Postgres-backed `WorkspaceStore`.

use std::path::PathBuf;

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use crate::error::{CtlError, Result};
use crate::workspaces::{Workspace, WorkspaceDomain, WorkspaceSpec, WorkspaceStore};

/// PG-backed workspace store. Soft-delete only — we never hard-delete rows,
/// so foreign-keyed snapshots can keep pointing at history.
pub struct PgWorkspaceStore {
    pool: PgPool,
}

impl PgWorkspaceStore {
    /// New store over an open pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_workspace(row: &sqlx::postgres::PgRow) -> Result<Workspace> {
    let config_json: serde_json::Value = row.get("config");
    let config: WorkspaceSpec = serde_json::from_value(config_json)
        .map_err(|e| CtlError::Internal(format!("workspace.config decode: {e}")))?;
    let domains: Vec<WorkspaceDomain> = row
        .try_get::<serde_json::Value, _>("domains")
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    Ok(Workspace {
        id:            row.get::<String, _>("id"),
        // `try_get` so the reader still works against a DB that hasn't
        // run the tenancy migration yet — stale rows default to `"dev"`.
        tenant_id:     row
            .try_get::<String, _>("tenant_id")
            .unwrap_or_else(|_| "dev".to_string()),
        created_at:    row.get::<OffsetDateTime, _>("created_at"),
        deleted_at:    row.get::<Option<OffsetDateTime>, _>("deleted_at"),
        source_dir:    PathBuf::from(row.get::<String, _>("source_dir")),
        output_dir:    PathBuf::from(row.get::<String, _>("output_dir")),
        config,
        git_repo:      row.get::<Option<String>, _>("git_repo"),
        git_ref:       row.get::<Option<String>, _>("git_ref"),
        label:         row.get::<Option<String>, _>("label"),
        domains,
        last_exec_at:  row.try_get::<Option<OffsetDateTime>, _>("last_exec_at").ok().flatten(),
        paused_at:     row.try_get::<Option<OffsetDateTime>, _>("paused_at").ok().flatten(),
        auto_snapshot: row.try_get::<Option<String>, _>("auto_snapshot").ok().flatten(),
    })
}

const WORKSPACE_COLS: &str =
    "id, tenant_id, created_at, deleted_at, source_dir, output_dir, config, \
     git_repo, git_ref, label, domains, last_exec_at, paused_at, auto_snapshot";

#[async_trait]
impl WorkspaceStore for PgWorkspaceStore {
    async fn insert(&self, ws: Workspace) -> Result<()> {
        let config_json = serde_json::to_value(&ws.config)
            .map_err(|e| CtlError::Internal(format!("workspace.config encode: {e}")))?;
        let domains_json = serde_json::to_value(&ws.domains)
            .map_err(|e| CtlError::Internal(format!("workspace.domains encode: {e}")))?;
        let r = sqlx::query(
            "INSERT INTO workspaces
                (id, tenant_id, created_at, deleted_at, source_dir, output_dir, config,
                 git_repo, git_ref, label, domains)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&ws.id)
        .bind(&ws.tenant_id)
        .bind(ws.created_at)
        .bind(ws.deleted_at)
        .bind(ws.source_dir.to_string_lossy().as_ref())
        .bind(ws.output_dir.to_string_lossy().as_ref())
        .bind(&config_json)
        .bind(ws.git_repo.as_deref())
        .bind(ws.git_ref.as_deref())
        .bind(ws.label.as_deref())
        .bind(&domains_json)
        .execute(&self.pool)
        .await;

        match r {
            Ok(_) => Ok(()),
            // 23505 = unique_violation on PRIMARY KEY.
            Err(sqlx::Error::Database(dbe)) if dbe.code().as_deref() == Some("23505") => {
                Err(CtlError::Conflict(format!("workspace {} already exists", ws.id)))
            }
            Err(e) => Err(CtlError::Internal(format!("workspace insert: {e}"))),
        }
    }

    async fn get(&self, id: &str) -> Option<Workspace> {
        let q = format!(
            "SELECT {WORKSPACE_COLS}
             FROM workspaces WHERE id = $1 AND deleted_at IS NULL",
        );
        let row = sqlx::query(&q)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()?;
        row_to_workspace(&row).ok()
    }

    async fn by_domain(&self, host: &str) -> Option<(Workspace, WorkspaceDomain)> {
        // GIN lookup: find a live row whose `domains` array contains a
        // matching object. The JSON comparison is case-sensitive here;
        // the caller is expected to normalize the host to lowercase and
        // to store domains in lowercase too (the route helper does so).
        let needle = serde_json::json!([{ "domain": host.to_ascii_lowercase() }]);
        let q = format!(
            "SELECT {WORKSPACE_COLS}
             FROM workspaces
             WHERE deleted_at IS NULL AND domains @> $1
             LIMIT 1",
        );
        let row = sqlx::query(&q)
            .bind(&needle)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()?;
        let ws = row_to_workspace(&row).ok()?;
        let host_lc = host.to_ascii_lowercase();
        let dom = ws
            .domains
            .iter()
            .find(|d| d.domain.eq_ignore_ascii_case(&host_lc))?
            .clone();
        Some((ws, dom))
    }

    async fn list(
        &self,
        tenant: Option<&str>,
        limit: usize,
        offset: usize,
        q: Option<&str>,
    ) -> (Vec<Workspace>, u64) {
        let limit_i  = limit.clamp(1, 500) as i64;
        let offset_i = offset as i64;
        let needle   = q.map(|s| s.trim()).filter(|s| !s.is_empty());

        // `tenant` IS NULL ⇒ no restriction (admin); otherwise equals.
        // A single COALESCE-style bind per branch keeps SQL readable
        // without dynamic query construction.
        let tenant_filter_sql =
            if tenant.is_some() { "AND tenant_id = $1" } else { "" };

        let (total, rows) = match needle {
            None => {
                let count_sql = format!(
                    "SELECT COUNT(*) FROM workspaces
                     WHERE deleted_at IS NULL {tenant_filter_sql}",
                );
                let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
                if let Some(t) = tenant { count_q = count_q.bind(t); }
                let total = count_q.fetch_one(&self.pool).await.unwrap_or(0);

                let (pos_limit, pos_offset) = if tenant.is_some() { ("$2", "$3") } else { ("$1", "$2") };
                let sql = format!(
                    "SELECT {WORKSPACE_COLS}
                     FROM workspaces
                     WHERE deleted_at IS NULL {tenant_filter_sql}
                     ORDER BY created_at DESC
                     LIMIT {pos_limit} OFFSET {pos_offset}",
                );
                let mut rows_q = sqlx::query(&sql);
                if let Some(t) = tenant { rows_q = rows_q.bind(t); }
                let rows = rows_q
                    .bind(limit_i)
                    .bind(offset_i)
                    .fetch_all(&self.pool)
                    .await
                    .unwrap_or_default();
                (total, rows)
            }
            Some(n) => {
                // `%needle%` — escape `%` and `_` so users can't
                // accidentally match everything.
                let pat = format!("%{}%", n.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
                let (pat_pos, lim_pos, off_pos) =
                    if tenant.is_some() { ("$2", "$3", "$4") } else { ("$1", "$2", "$3") };
                let count_sql = format!(
                    "SELECT COUNT(*) FROM workspaces
                     WHERE deleted_at IS NULL {tenant_filter_sql}
                       AND ({pat_pos} IS NOT NULL
                            AND (id ILIKE {pat_pos} ESCAPE '\\' OR label ILIKE {pat_pos} ESCAPE '\\' OR git_repo ILIKE {pat_pos} ESCAPE '\\'))",
                );
                let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
                if let Some(t) = tenant { count_q = count_q.bind(t); }
                let total = count_q.bind(&pat).fetch_one(&self.pool).await.unwrap_or(0);

                let sql = format!(
                    "SELECT {WORKSPACE_COLS}
                     FROM workspaces
                     WHERE deleted_at IS NULL {tenant_filter_sql}
                       AND (id ILIKE {pat_pos} ESCAPE '\\' OR label ILIKE {pat_pos} ESCAPE '\\' OR git_repo ILIKE {pat_pos} ESCAPE '\\')
                     ORDER BY created_at DESC
                     LIMIT {lim_pos} OFFSET {off_pos}",
                );
                let mut rows_q = sqlx::query(&sql);
                if let Some(t) = tenant { rows_q = rows_q.bind(t); }
                let rows = rows_q
                    .bind(&pat)
                    .bind(limit_i)
                    .bind(offset_i)
                    .fetch_all(&self.pool)
                    .await
                    .unwrap_or_default();
                (total, rows)
            }
        };

        let parsed: Vec<Workspace> = rows.iter().filter_map(|r| row_to_workspace(r).ok()).collect();
        (parsed, total as u64)
    }

    async fn mark_deleted(&self, id: &str) -> Option<Workspace> {
        let q = format!(
            "UPDATE workspaces
             SET deleted_at = now()
             WHERE id = $1 AND deleted_at IS NULL
             RETURNING {WORKSPACE_COLS}",
        );
        let row = sqlx::query(&q)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()?;
        row_to_workspace(&row).ok()
    }

    async fn set_label(&self, id: &str, label: Option<&str>) -> Option<Workspace> {
        let q = format!(
            "UPDATE workspaces
             SET label = $2
             WHERE id = $1 AND deleted_at IS NULL
             RETURNING {WORKSPACE_COLS}",
        );
        let row = sqlx::query(&q)
            .bind(id)
            .bind(label)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()?;
        row_to_workspace(&row).ok()
    }

    async fn touch(&self, id: &str) {
        let _ = sqlx::query(
            "UPDATE workspaces SET last_exec_at = now() WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await;
    }

    async fn mark_paused(&self, id: &str, auto_snapshot: &str) {
        let _ = sqlx::query(
            "UPDATE workspaces
             SET paused_at = now(), auto_snapshot = $2
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(auto_snapshot)
        .execute(&self.pool)
        .await;
    }

    async fn mark_resumed(&self, id: &str) -> Option<String> {
        // `UPDATE ... RETURNING` yields post-update values in PG, but we
        // want the snapshot id that was stored *before* the clear — that's
        // what the caller needs in order to delete the auto-snapshot.
        // A CTE captures the pre-update value in the same round trip.
        let row = sqlx::query(
            "WITH prev AS (
                SELECT auto_snapshot FROM workspaces
                WHERE id = $1 AND deleted_at IS NULL
                FOR UPDATE
            )
            UPDATE workspaces
            SET    paused_at = NULL, auto_snapshot = NULL
            FROM   prev
            WHERE  workspaces.id = $1
            RETURNING prev.auto_snapshot AS prev_snap",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()?;
        row.try_get::<Option<String>, _>("prev_snap").ok().flatten()
    }
}
