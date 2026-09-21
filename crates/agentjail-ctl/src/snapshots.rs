//! Named snapshots of workspace output dirs.
//!
//! A snapshot is a copy of a workspace's `output_dir` taken at a point in
//! time. The engine's [`agentjail::Snapshot`] does the filesystem work
//! (copy or COW, symlink-safe). This module is the control-plane ledger
//! that tracks snapshot metadata in memory or Postgres.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{CtlError, Result};

/// One snapshot row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRecord {
    /// Opaque id, `snap_<hex>`.
    pub id: String,
    /// Tenant that owns the snapshot. Inherited from the parent
    /// workspace at capture time; route-layer filters make cross-tenant
    /// reads invisible.
    pub tenant_id: String,
    /// The workspace this snapshot was taken from. `None` if the parent
    /// workspace was hard-deleted (ON DELETE SET NULL in Postgres).
    pub workspace_id: Option<String>,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// When the snapshot was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Absolute path on disk under `state_dir/snapshots/<id>/`.
    pub path: PathBuf,
    /// Size of the snapshot dir in bytes, reported by
    /// [`agentjail::Snapshot::size_bytes`].
    pub size_bytes: u64,
}

/// Contract for snapshot persistence. Mirrors the shape of other stores.
#[async_trait]
pub trait SnapshotStore: Send + Sync + 'static {
    /// Insert a new record.
    async fn insert(&self, snap: SnapshotRecord) -> Result<()>;
    /// Fetch by id.
    async fn get(&self, id: &str) -> Option<SnapshotRecord>;
    /// List, newest first.
    ///
    /// `tenant`: `Some(id)` restricts to a single tenant (operator
    /// role); `None` returns every tenant's rows (admin).
    ///
    /// `workspace_id`: narrow further to one workspace. `q`: substring
    /// match on id/name/workspace_id. All filters compose; `total`
    /// reflects the fully-filtered count.
    async fn list(
        &self,
        tenant: Option<&str>,
        workspace_id: Option<&str>,
        limit: usize,
        offset: usize,
        q: Option<&str>,
    ) -> (Vec<SnapshotRecord>, u64);
    /// Remove a row and return it.
    async fn remove(&self, id: &str) -> Option<SnapshotRecord>;
}

/// Generate a new snapshot id: `snap_<24hex>`.
#[must_use]
pub fn new_snapshot_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    format!("snap_{}", hex::encode(b))
}

// ---------- in-memory impl ----------

/// Default in-memory snapshot store.
#[derive(Default)]
pub struct InMemorySnapshotStore {
    inner: RwLock<HashMap<String, SnapshotRecord>>,
}

impl InMemorySnapshotStore {
    /// New, empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SnapshotStore for InMemorySnapshotStore {
    async fn insert(&self, snap: SnapshotRecord) -> Result<()> {
        let mut g = self
            .inner
            .write()
            .map_err(|_| CtlError::Internal("snapshot store poisoned".into()))?;
        if g.contains_key(&snap.id) {
            return Err(CtlError::Conflict(format!(
                "snapshot {} already exists",
                snap.id
            )));
        }
        g.insert(snap.id.clone(), snap);
        Ok(())
    }

    async fn get(&self, id: &str) -> Option<SnapshotRecord> {
        self.inner.read().ok()?.get(id).cloned()
    }

    async fn list(
        &self,
        tenant: Option<&str>,
        workspace_id: Option<&str>,
        limit: usize,
        offset: usize,
        q: Option<&str>,
    ) -> (Vec<SnapshotRecord>, u64) {
        let Ok(g) = self.inner.read() else {
            return (Vec::new(), 0);
        };
        let needle = q.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let mut rows: Vec<SnapshotRecord> = g
            .values()
            .filter(|r| match tenant {
                Some(t) => r.tenant_id == t,
                None => true,
            })
            .filter(|r| match workspace_id {
                Some(w) => r.workspace_id.as_deref() == Some(w),
                None => true,
            })
            .filter(|r| match &needle {
                None => true,
                Some(n) =>
                    r.id.to_lowercase().contains(n)
                    || r.name.as_deref().map(|s| s.to_lowercase().contains(n)).unwrap_or(false)
                    || r.workspace_id.as_deref().map(|s| s.to_lowercase().contains(n)).unwrap_or(false),
            })
            .cloned()
            .collect();
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let total = rows.len() as u64;
        let page = rows
            .into_iter()
            .skip(offset)
            .take(limit.clamp(1, 500))
            .collect();
        (page, total)
    }

    async fn remove(&self, id: &str) -> Option<SnapshotRecord> {
        self.inner.write().ok()?.remove(id)
    }
}

// ---------- retention / gc ----------

/// GC policy for the snapshot store.
///
/// The sweeper runs on [`gc::spawn_sweeper`] in the background, deleting
/// snapshots that exceed either the age or the count cap. Snapshots are
/// removed oldest-first; on-disk dirs are always unlinked before the DB
/// row, so partial failures leak rows (recoverable) not disk (not).
#[derive(Debug, Clone, Copy, Default)]
pub struct SnapshotGcConfig {
    /// Drop snapshots older than this many seconds. `None` = no age cap.
    pub max_age_secs: Option<u64>,
    /// Keep at most this many snapshots. `None` = no count cap.
    pub max_count: Option<usize>,
    /// How often the sweeper runs, in seconds. Defaults to 60.
    pub tick_secs: u64,
}

impl SnapshotGcConfig {
    /// True when any limit is configured (the sweeper is worth running).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.max_age_secs.is_some() || self.max_count.is_some()
    }
}

/// Background sweeper utilities for the snapshot store.
pub mod gc {
    use super::{SnapshotGcConfig, SnapshotRecord, SnapshotStore};
    use std::sync::Arc;
    use time::OffsetDateTime;

    /// Run a single GC pass. Returns the number of snapshots deleted.
    /// Callers that want continuous GC should use [`spawn_sweeper`].
    pub async fn run_once(
        store: &dyn SnapshotStore,
        cfg: &SnapshotGcConfig,
    ) -> usize {
        if !cfg.is_enabled() {
            return 0;
        }

        // Pull up to 10k rows — enough to observe the full set for most
        // deployments. Oversized installations should prefer an external
        // retention job.
        // GC sweeper runs unscoped — it's an admin-level background job
        // that evicts across every tenant based on the same policy.
        let (rows, _total) = store.list(None, None, 10_000, 0, None).await;

        let mut to_delete: Vec<SnapshotRecord> = Vec::new();

        if let Some(max_age_secs) = cfg.max_age_secs {
            let cutoff = OffsetDateTime::now_utc()
                - time::Duration::seconds(max_age_secs as i64);
            to_delete.extend(rows.iter().filter(|r| r.created_at < cutoff).cloned());
        }

        if let Some(max_count) = cfg.max_count
            && rows.len() > max_count
        {
            // `list` returns newest-first. Evict the oldest tail.
            let surplus = rows.len() - max_count;
            to_delete.extend(rows.iter().rev().take(surplus).cloned());
        }

        // Deduplicate by id (age + count caps can overlap).
        to_delete.sort_by(|a, b| a.id.cmp(&b.id));
        to_delete.dedup_by(|a, b| a.id == b.id);

        let mut deleted = 0;
        for r in to_delete {
            // Best-effort disk removal first.
            if r.path.exists() {
                let _ = std::fs::remove_dir_all(&r.path);
            }
            if store.remove(&r.id).await.is_some() {
                deleted += 1;
            }
        }
        deleted
    }

    /// Spawn a long-running sweeper on the current tokio runtime. The
    /// returned `JoinHandle` lets the caller abort on shutdown.
    pub fn spawn_sweeper(
        store: Arc<dyn SnapshotStore>,
        cfg: SnapshotGcConfig,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if !cfg.is_enabled() {
            return None;
        }
        let tick = std::time::Duration::from_secs(cfg.tick_secs.max(1));
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await; // skip the immediate tick
            loop {
                interval.tick().await;
                let n = run_once(store.as_ref(), &cfg).await;
                if n > 0 {
                    tracing::info!(
                        deleted = n,
                        max_age_secs = ?cfg.max_age_secs,
                        max_count = ?cfg.max_count,
                        "snapshot gc sweep"
                    );
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, ws: Option<&str>) -> SnapshotRecord {
        sample_in(id, ws, "dev")
    }

    fn sample_in(id: &str, ws: Option<&str>, tenant: &str) -> SnapshotRecord {
        SnapshotRecord {
            id: id.into(),
            tenant_id: tenant.into(),
            workspace_id: ws.map(str::to_string),
            name: None,
            created_at: OffsetDateTime::now_utc(),
            path: PathBuf::from(format!("/tmp/snap/{id}")),
            size_bytes: 1024,
        }
    }

    #[tokio::test]
    async fn insert_get_remove() {
        let store = InMemorySnapshotStore::new();
        store.insert(sample("snap_a", Some("wrk_a"))).await.unwrap();
        assert!(store.get("snap_a").await.is_some());
        assert!(store.remove("snap_a").await.is_some());
        assert!(store.get("snap_a").await.is_none());
    }

    #[tokio::test]
    async fn list_filters_by_workspace() {
        let store = InMemorySnapshotStore::new();
        store.insert(sample("snap_a", Some("wrk_x"))).await.unwrap();
        store.insert(sample("snap_b", Some("wrk_y"))).await.unwrap();
        store.insert(sample("snap_c", Some("wrk_x"))).await.unwrap();

        let (rows, total) = store.list(None, Some("wrk_x"), 100, 0, None).await;
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r.workspace_id.as_deref(), Some("wrk_x"));
        }
    }

    #[tokio::test]
    async fn list_q_filters_on_id_name_and_workspace_id() {
        let store = InMemorySnapshotStore::new();
        let mut one = sample("snap_baseline_a", Some("wrk_1"));
        one.name = Some("baseline".into());
        store.insert(one).await.unwrap();

        let mut two = sample("snap_after_bun_install", Some("wrk_2"));
        two.name = Some("after-bun-install".into());
        store.insert(two).await.unwrap();

        // match on id — "baseline" hits snap_baseline_a's id AND its
        // own name, but that's still one row.
        let (rows, total) = store.list(None, None, 100, 0, Some("baseline")).await;
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "snap_baseline_a");

        // match on name only
        let (rows, total) = store.list(None, None, 100, 0, Some("after-bun")).await;
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "snap_after_bun_install");

        // match on workspace_id
        let (rows, total) = store.list(None, None, 100, 0, Some("wrk_2")).await;
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "snap_after_bun_install");

        // workspace_id filter + q combine (intersection)
        let (_, total) = store.list(None, Some("wrk_1"), 100, 0, Some("baseline")).await;
        assert_eq!(total, 1);

        // empty needle = no filter
        let (_, total) = store.list(None, None, 100, 0, Some("  ")).await;
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn list_tenant_filter_isolates_rows() {
        let store = InMemorySnapshotStore::new();
        store.insert(sample_in("snap_a", Some("wrk_a"), "acme")).await.unwrap();
        store.insert(sample_in("snap_b", Some("wrk_b"), "other")).await.unwrap();
        store.insert(sample_in("snap_c", Some("wrk_c"), "acme")).await.unwrap();

        // Admin scope — all rows.
        let (_, total) = store.list(None, None, 100, 0, None).await;
        assert_eq!(total, 3);

        // Operator scope — only their tenant.
        let (rows, total) = store.list(Some("acme"), None, 100, 0, None).await;
        assert_eq!(total, 2);
        for r in &rows {
            assert_eq!(r.tenant_id, "acme");
        }

        // Tenant filter + workspace filter compose — cross-tenant
        // workspace id returns nothing even if the id matches.
        let (_, total) = store.list(Some("acme"), Some("wrk_b"), 100, 0, None).await;
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn duplicate_insert_is_conflict() {
        let store = InMemorySnapshotStore::new();
        store.insert(sample("snap_a", None)).await.unwrap();
        let err = store.insert(sample("snap_a", None)).await.unwrap_err();
        assert!(matches!(err, CtlError::Conflict(_)));
    }

    // ---------- gc ----------

    fn sample_at(id: &str, created_at: OffsetDateTime) -> SnapshotRecord {
        SnapshotRecord {
            id: id.into(),
            tenant_id: "dev".into(),
            workspace_id: None,
            name: None,
            created_at,
            path: PathBuf::from(format!("/tmp/snap/{id}")),
            size_bytes: 0,
        }
    }

    #[tokio::test]
    async fn gc_disabled_is_noop() {
        let store = InMemorySnapshotStore::new();
        store.insert(sample("snap_a", None)).await.unwrap();
        let n = gc::run_once(&store, &SnapshotGcConfig::default()).await;
        assert_eq!(n, 0);
        assert!(store.get("snap_a").await.is_some());
    }

    #[tokio::test]
    async fn gc_prunes_by_age() {
        let store = InMemorySnapshotStore::new();
        let old = OffsetDateTime::now_utc() - time::Duration::seconds(120);
        let new = OffsetDateTime::now_utc();
        store.insert(sample_at("snap_old", old)).await.unwrap();
        store.insert(sample_at("snap_new", new)).await.unwrap();

        let cfg = SnapshotGcConfig {
            max_age_secs: Some(60),
            max_count: None,
            tick_secs: 60,
        };
        let n = gc::run_once(&store, &cfg).await;
        assert_eq!(n, 1);
        assert!(store.get("snap_old").await.is_none());
        assert!(store.get("snap_new").await.is_some());
    }

    #[tokio::test]
    async fn gc_prunes_by_count() {
        let store = InMemorySnapshotStore::new();
        let base = OffsetDateTime::now_utc();
        for i in 0..5 {
            let t = base + time::Duration::seconds(i as i64);
            store.insert(sample_at(&format!("snap_{i}"), t)).await.unwrap();
        }
        let cfg = SnapshotGcConfig {
            max_age_secs: None,
            max_count: Some(3),
            tick_secs: 60,
        };
        let n = gc::run_once(&store, &cfg).await;
        assert_eq!(n, 2);
        // Oldest two (snap_0, snap_1) should be gone; 2,3,4 retained.
        assert!(store.get("snap_0").await.is_none());
        assert!(store.get("snap_1").await.is_none());
        assert!(store.get("snap_2").await.is_some());
        assert!(store.get("snap_4").await.is_some());
    }

    #[tokio::test]
    async fn gc_age_and_count_overlap_does_not_double_delete() {
        let store = InMemorySnapshotStore::new();
        let old = OffsetDateTime::now_utc() - time::Duration::seconds(120);
        store.insert(sample_at("snap_a", old)).await.unwrap();
        store.insert(sample_at("snap_b", old)).await.unwrap();
        store.insert(sample("snap_c", None)).await.unwrap();

        let cfg = SnapshotGcConfig {
            max_age_secs: Some(60),
            max_count:    Some(1),
            tick_secs:    60,
        };
        let n = gc::run_once(&store, &cfg).await;
        // snap_a and snap_b match both age (old) and count (over by 2);
        // must count as 2 deletions, not 4.
        assert_eq!(n, 2);
        assert!(store.get("snap_a").await.is_none());
        assert!(store.get("snap_b").await.is_none());
        assert!(store.get("snap_c").await.is_some());
    }
}
