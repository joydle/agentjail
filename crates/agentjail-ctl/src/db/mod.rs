//! Postgres backing for the control plane.
//!
//! All three write-heavy stores (credentials, audit, jails) get a concrete
//! Postgres implementation of their existing trait, so callers don't need
//! to know which backend is in use. Switched on via
//! `ControlPlaneConfig::database_url`; when `None`, the in-memory stores
//! remain the default.

mod audit_pg;
mod credentials_pg;
mod jails_pg;
mod sessions_pg;
mod snapshots_pg;
mod tokens_pg;
mod workspaces_pg;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

pub use audit_pg::PgAuditStore;
pub use credentials_pg::{PgCredentialStore, rehydrate_keystore};
pub use jails_pg::PgJailStore;
pub use sessions_pg::{PgSessionStore, sweep_expired_sessions};
pub use snapshots_pg::PgSnapshotStore;
pub use tokens_pg::{PgTokenStore, sweep_expired_tokens};
pub use workspaces_pg::PgWorkspaceStore;

/// Migrations applied at startup, in order. Each file is expected to be
/// idempotent (`CREATE … IF NOT EXISTS`, `ALTER TABLE … ADD COLUMN IF
/// NOT EXISTS`) so replays on already-migrated databases are no-ops.
///
/// Add new migrations at the end. Renumber only if you have to reorder
/// a never-deployed change — once a migration has run in any
/// environment it's effectively frozen.
const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../../migrations/0001_init.sql")),
    (
        "0002_workspaces_snapshots",
        include_str!("../../migrations/0002_workspaces_snapshots.sql"),
    ),
    (
        "0003_workspace_idle",
        include_str!("../../migrations/0003_workspace_idle.sql"),
    ),
    (
        "0004_workspace_domains",
        include_str!("../../migrations/0004_workspace_domains.sql"),
    ),
    (
        "0005_jail_config_snapshot",
        include_str!("../../migrations/0005_jail_config_snapshot.sql"),
    ),
    (
        "0006_jail_live_stats",
        include_str!("../../migrations/0006_jail_live_stats.sql"),
    ),
    (
        "0007_tenant_id",
        include_str!("../../migrations/0007_tenant_id.sql"),
    ),
    (
        "0008_credentials_tenant",
        include_str!("../../migrations/0008_credentials_tenant.sql"),
    ),
    (
        "0009_sessions_tokens",
        include_str!("../../migrations/0009_sessions_tokens.sql"),
    ),
    (
        "0010_workspace_domains_unique",
        include_str!("../../migrations/0010_workspace_domains_unique.sql"),
    ),
    (
        "0011_phantom_token_sha256",
        include_str!("../../migrations/0011_phantom_token_sha256.sql"),
    ),
];

/// Connect to Postgres + run every embedded migration. Idempotent.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(3))
        .connect(database_url)
        .await?;

    // Serialise migrations across processes: concurrent `CREATE TABLE IF
    // NOT EXISTS` from two replicas (or two test binaries) starting at
    // once can still race in the catalog (`pg_type_typname_nsp_index`).
    // The advisory lock is session-scoped, so everything runs on one
    // connection and the lock dies with it if we bail out.
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(&mut *conn)
        .await?;
    let mut result = Ok(());
    for (name, sql) in MIGRATIONS {
        tracing::debug!(migration = name, "applying");
        if let Err(e) = sqlx::raw_sql(sql).execute(&mut *conn).await {
            result = Err(e);
            break;
        }
    }
    let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(&mut *conn)
        .await;
    result?;
    if unlock.is_err() {
        // Don't hand a connection that may still hold the lock back to
        // the pool; closing it releases the lock server-side.
        conn.detach();
    }

    Ok(pool)
}

/// `pg_advisory_lock` key guarding [`MIGRATIONS`] ("ajmigr" in ASCII).
const MIGRATION_LOCK_KEY: i64 = 0x616a_6d69_6772;
