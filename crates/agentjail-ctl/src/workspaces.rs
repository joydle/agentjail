//! Persistent workspaces — long-lived mount trees that survive across HTTP
//! requests. Each `POST /v1/workspaces/:id/exec` spawns a fresh jail
//! against the same `source` + `output` directories, so filesystem mutations
//! persist between calls.
//!
//! This module defines the domain types ([`Workspace`], [`WorkspaceSpec`])
//! and the [`WorkspaceStore`] trait. The in-memory implementation is here;
//! the Postgres implementation lives in [`crate::db::workspaces_pg`].

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{CtlError, Result};

/// A persistent workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    /// Opaque identifier, `wrk_<hex>`.
    pub id: String,
    /// Tenant that owns this workspace. Stamped from the caller's
    /// [`crate::tenant::TenantScope`] at create time and immutable
    /// thereafter; cross-tenant reads are filtered out at the route
    /// layer.
    pub tenant_id: String,
    /// When the workspace was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Soft-delete marker; when set, `source_dir`/`output_dir` are gone.
    #[serde(with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
    /// Absolute path to the jail's source mount.
    pub source_dir: PathBuf,
    /// Absolute path to the jail's output mount (read-write, persistent).
    pub output_dir: PathBuf,
    /// The full jail config chosen at creation time (network policy,
    /// seccomp, cpu/memory limits, timeouts). Re-applied on every exec.
    pub config: WorkspaceSpec,
    /// Optional provenance: the repo that was cloned into `source_dir`.
    pub git_repo: Option<String>,
    /// Optional git ref (branch/tag/commit).
    pub git_ref: Option<String>,
    /// Human-readable label.
    pub label: Option<String>,
    /// Hostname → backend-URL forwards handled by the gateway listener
    /// (opt-in via `AGENTJAIL_GATEWAY_ADDR`). Empty by default.
    #[serde(default)]
    pub domains: Vec<WorkspaceDomain>,
    /// When the most recent exec started. Drives idle-timeout detection.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_exec_at: Option<OffsetDateTime>,
    /// When the reaper paused this workspace. `None` = active.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub paused_at: Option<OffsetDateTime>,
    /// Snapshot id the reaper captured before wiping `output_dir`. The
    /// next exec restores from this before running; the field clears on
    /// resume.
    #[serde(default)]
    pub auto_snapshot: Option<String>,
}

/// One hostname-routed entry exposed through the agentjail-server
/// gateway listener. Exactly one of `backend_url` / `vm_port` must be
/// set; the two forms are:
///
/// * `backend_url` — static URL the caller already wired to a
///   reachable address (e.g. a sidecar or host-forwarded port).
/// * `vm_port` — a port bound *inside* the workspace's jail. The
///   gateway resolves this to `http://<live_jail_ip>:<vm_port>/` at
///   request time by consulting [`ActiveJailIps`]. Returns 503 when
///   no exec is in flight (dev server not running yet).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDomain {
    /// Hostname the gateway matches against the `Host` header (case-
    /// insensitive, no port).
    pub domain: String,
    /// Static URL target. Mutually exclusive with `vm_port`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_url: Option<String>,
    /// Jail-internal port. Mutually exclusive with `backend_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_port: Option<u16>,
}

/// Decoded target form of a [`WorkspaceDomain`] — cheap to compute,
/// lets the gateway branch without re-validating the underlying shape.
#[derive(Debug, Clone)]
pub enum WorkspaceDomainTarget {
    /// Caller-supplied static URL.
    BackendUrl(String),
    /// Jail-internal port; resolved at request time.
    VmPort(u16),
}

impl WorkspaceDomain {
    /// Validate exactly-one-of-backend_url/vm_port and return the
    /// decoded target. Called by the create path so bad domains fail
    /// up front rather than silently 503ing at request time.
    pub fn target(&self) -> Result<WorkspaceDomainTarget> {
        match (self.backend_url.as_deref(), self.vm_port) {
            (Some(url), None) => {
                if url.starts_with("http://") || url.starts_with("https://") {
                    Ok(WorkspaceDomainTarget::BackendUrl(url.to_string()))
                } else {
                    Err(CtlError::BadRequest(format!(
                        "backend_url must start with http:// or https:// (got {url:?})",
                    )))
                }
            }
            (None, Some(p)) => {
                if p == 0 {
                    Err(CtlError::BadRequest("vm_port must be > 0".into()))
                } else {
                    Ok(WorkspaceDomainTarget::VmPort(p))
                }
            }
            (Some(_), Some(_)) => Err(CtlError::BadRequest(
                "workspace domain: set exactly one of backend_url / vm_port".into(),
            )),
            (None, None) => Err(CtlError::BadRequest(
                "workspace domain: one of backend_url / vm_port is required".into(),
            )),
        }
    }
}

/// Serializable subset of jail options persisted with a workspace.
///
/// We intentionally keep this slimmer than `agentjail::JailConfig` — the
/// source/output/workdir paths are derived from the workspace id, and
/// environment variables are merged per-exec.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceSpec {
    /// Memory limit in MB.
    pub memory_mb: u64,
    /// Timeout (seconds) applied to each exec.
    pub timeout_secs: u64,
    /// CPU quota percent (100 = one full core).
    pub cpu_percent: u64,
    /// Process count cap.
    pub max_pids: u64,
    /// Network policy tag (`"none" | "loopback" | "allowlist"`).
    pub network_mode: String,
    /// Domains when `network_mode == "allowlist"`.
    #[serde(default)]
    pub network_domains: Vec<String>,
    /// Seccomp profile (`"standard" | "strict"`).
    pub seccomp: String,
    /// When `> 0`, the reaper pauses this workspace after it goes idle for
    /// this many seconds. `0` = never auto-pause.
    #[serde(default)]
    pub idle_timeout_secs: u64,
    /// Flavor names this workspace expects at exec time. Each name is
    /// resolved against [`crate::FlavorRegistry`] into a host directory
    /// that gets bind-mounted read-only into the jail. The stored list
    /// is the names (stable across flavor-directory changes), not the
    /// resolved paths (which depend on deployment layout).
    #[serde(default)]
    pub flavors: Vec<String>,
}

/// Contract for workspace persistence. Mirrors [`crate::SessionStore`] and
/// [`crate::JailStore`] shape so the server can swap an in-memory impl for
/// a Postgres-backed one via `ControlPlane::with_postgres`.
#[async_trait]
pub trait WorkspaceStore: Send + Sync + 'static {
    /// Insert a new workspace. Returns an error if the id already exists.
    async fn insert(&self, ws: Workspace) -> Result<()>;
    /// Fetch by id. Deleted workspaces return `None` (soft-delete).
    async fn get(&self, id: &str) -> Option<Workspace>;
    /// Find the first live workspace that declares `host` in its
    /// `domains` list. Case-insensitive match on the `domain` field.
    /// Used by the gateway listener to route incoming requests.
    async fn by_domain(&self, host: &str) -> Option<(Workspace, WorkspaceDomain)>;
    /// List live workspaces, newest first. `limit` capped at 500.
    ///
    /// `tenant`: `Some(id)` restricts the result to rows stamped with
    /// that tenant — used by the operator role. `None` returns every
    /// tenant's rows — used by admins.
    ///
    /// `q`: case-insensitive substring match on `id` / `label` /
    /// `git_repo`. Applied after the tenant filter; `total` reflects
    /// the filtered count.
    async fn list(
        &self,
        tenant: Option<&str>,
        limit: usize,
        offset: usize,
        q: Option<&str>,
    ) -> (Vec<Workspace>, u64);
    /// Mark deleted + set `deleted_at = now()`.
    async fn mark_deleted(&self, id: &str) -> Option<Workspace>;
    /// Update a workspace's human-readable label. `None` clears it.
    /// Returns the refreshed record, or `None` if the id is unknown or
    /// soft-deleted.
    async fn set_label(&self, id: &str, label: Option<&str>) -> Option<Workspace>;
    /// Bump `last_exec_at`. Called on every successful exec dispatch.
    async fn touch(&self, id: &str);
    /// Mark paused with an auto-snapshot id.
    async fn mark_paused(&self, id: &str, auto_snapshot: &str);
    /// Clear `paused_at` + `auto_snapshot` after a restore. Returns the
    /// snapshot id that was cleared (so the caller can delete it, or
    /// re-use it on failure).
    async fn mark_resumed(&self, id: &str) -> Option<String>;
}

/// Generate a new workspace id: `wrk_<24hex>`.
#[must_use]
pub fn new_workspace_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    format!("wrk_{}", hex::encode(b))
}

/// Per-workspace exclusion for filesystem writes.
///
/// A workspace's `output_dir` is a shared mount across execs. To keep it
/// coherent we hand out one async mutex per workspace id. `try_lock` on
/// this mutex is how the exec + snapshot routes decide whether to run
/// serially or return 409.
///
/// This is ephemeral: the locks live as long as the process does. No
/// cross-restart coherence is needed — the OS file system is the ultimate
/// source of truth.
#[derive(Default)]
pub struct WorkspaceLocks {
    inner: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl WorkspaceLocks {
    /// New, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (or create) the mutex associated with a workspace id.
    #[must_use]
    pub fn lock_for(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.entry(id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Drop the mutex slot when a workspace is deleted so maps don't grow
    /// unbounded. Safe to call on a missing id.
    pub fn forget(&self, id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remove(id);
        }
    }
}

/// Tracks the cgroup path of each workspace's currently-running exec.
///
/// The snapshot route reads this to freeze-before-copy when a snapshot is
/// requested mid-exec. An entry exists only for the duration of the
/// in-flight exec; absence means "no freeze needed".
#[derive(Default)]
pub struct ActiveCgroups {
    inner: Mutex<HashMap<String, PathBuf>>,
}

impl ActiveCgroups {
    /// New, empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the cgroup path for a workspace's in-flight exec. Overwrites
    /// any stale entry for the same id.
    pub fn insert(&self, workspace_id: &str, cgroup_path: PathBuf) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(workspace_id.to_string(), cgroup_path);
        }
    }

    /// Drop the record after the exec completes.
    pub fn remove(&self, workspace_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remove(workspace_id);
        }
    }

    /// Current cgroup path for a workspace, if any exec is in flight.
    #[must_use]
    pub fn get(&self, workspace_id: &str) -> Option<PathBuf> {
        self.inner.lock().ok()?.get(workspace_id).cloned()
    }
}

/// Tracks the jail-side IPv4 address of each workspace's currently-
/// running exec (only populated when the exec used
/// `Network::Allowlist` — the only mode that gets a veth pair).
///
/// Read by the hostname-routed gateway to resolve `VmPort`-style
/// workspace domains to `http://<jail_ip>:<vm_port>/` at request time.
/// An entry exists only for the duration of the in-flight exec;
/// absence means "no live server, 503".
#[derive(Default)]
pub struct ActiveJailIps {
    inner: Mutex<HashMap<String, Ipv4Addr>>,
}

impl ActiveJailIps {
    /// New, empty tracker.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Record the jail IP for a workspace's in-flight exec.
    pub fn insert(&self, workspace_id: &str, ip: Ipv4Addr) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(workspace_id.to_string(), ip);
        }
    }

    /// Drop the record after the exec completes.
    pub fn remove(&self, workspace_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remove(workspace_id);
        }
    }

    /// Current jail IP for a workspace, if a network-enabled exec is
    /// in flight.
    #[must_use]
    pub fn get(&self, workspace_id: &str) -> Option<Ipv4Addr> {
        self.inner.lock().ok()?.get(workspace_id).copied()
    }
}

pub mod idle;
pub mod slug;

// ---------- in-memory impl ----------

/// Default in-memory workspace store. Data is lost on restart; Postgres is
/// the durable choice.
#[derive(Default)]
pub struct InMemoryWorkspaceStore {
    inner: RwLock<HashMap<String, Workspace>>,
}

impl InMemoryWorkspaceStore {
    /// New, empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl WorkspaceStore for InMemoryWorkspaceStore {
    async fn insert(&self, ws: Workspace) -> Result<()> {
        let mut g = self
            .inner
            .write()
            .map_err(|_| CtlError::Internal("workspace store poisoned".into()))?;
        if g.contains_key(&ws.id) {
            return Err(CtlError::Conflict(format!(
                "workspace {} already exists",
                ws.id
            )));
        }
        g.insert(ws.id.clone(), ws);
        Ok(())
    }

    async fn get(&self, id: &str) -> Option<Workspace> {
        let g = self.inner.read().ok()?;
        g.get(id).filter(|w| w.deleted_at.is_none()).cloned()
    }

    async fn by_domain(&self, host: &str) -> Option<(Workspace, WorkspaceDomain)> {
        let g = self.inner.read().ok()?;
        let host_lc = host.to_ascii_lowercase();
        for w in g.values() {
            if w.deleted_at.is_some() {
                continue;
            }
            for d in &w.domains {
                if d.domain.eq_ignore_ascii_case(&host_lc) {
                    return Some((w.clone(), d.clone()));
                }
            }
        }
        None
    }

    async fn list(
        &self,
        tenant: Option<&str>,
        limit: usize,
        offset: usize,
        q: Option<&str>,
    ) -> (Vec<Workspace>, u64) {
        let Ok(g) = self.inner.read() else {
            return (Vec::new(), 0);
        };
        let needle = q.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let mut live: Vec<Workspace> = g
            .values()
            .filter(|w| w.deleted_at.is_none())
            .filter(|w| match tenant {
                None    => true,
                Some(t) => w.tenant_id == t,
            })
            .filter(|w| match &needle {
                None => true,
                Some(n) =>
                    w.id.to_lowercase().contains(n)
                    || w.label.as_deref().map(|s| s.to_lowercase().contains(n)).unwrap_or(false)
                    || w.git_repo.as_deref().map(|s| s.to_lowercase().contains(n)).unwrap_or(false),
            })
            .cloned()
            .collect();
        live.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let total = live.len() as u64;
        let rows = live
            .into_iter()
            .skip(offset)
            .take(limit.clamp(1, 500))
            .collect();
        (rows, total)
    }

    async fn mark_deleted(&self, id: &str) -> Option<Workspace> {
        let mut g = self.inner.write().ok()?;
        let ws = g.get_mut(id)?;
        if ws.deleted_at.is_some() {
            return None;
        }
        ws.deleted_at = Some(OffsetDateTime::now_utc());
        Some(ws.clone())
    }

    async fn set_label(&self, id: &str, label: Option<&str>) -> Option<Workspace> {
        let mut g = self.inner.write().ok()?;
        let ws = g.get_mut(id)?;
        if ws.deleted_at.is_some() {
            return None;
        }
        ws.label = label.map(|s| s.to_string());
        Some(ws.clone())
    }

    async fn touch(&self, id: &str) {
        if let Ok(mut g) = self.inner.write()
            && let Some(ws) = g.get_mut(id)
        {
            ws.last_exec_at = Some(OffsetDateTime::now_utc());
        }
    }

    async fn mark_paused(&self, id: &str, auto_snapshot: &str) {
        if let Ok(mut g) = self.inner.write()
            && let Some(ws) = g.get_mut(id)
        {
            ws.paused_at = Some(OffsetDateTime::now_utc());
            ws.auto_snapshot = Some(auto_snapshot.to_string());
        }
    }

    async fn mark_resumed(&self, id: &str) -> Option<String> {
        let mut g = self.inner.write().ok()?;
        let ws = g.get_mut(id)?;
        let snap = ws.auto_snapshot.take();
        ws.paused_at = None;
        snap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> Workspace {
        sample_in(id, "dev")
    }

    fn sample_in(id: &str, tenant: &str) -> Workspace {
        Workspace {
            id: id.into(),
            tenant_id: tenant.into(),
            created_at: OffsetDateTime::now_utc(),
            deleted_at: None,
            source_dir: PathBuf::from(format!("/tmp/wrk/{id}/source")),
            output_dir: PathBuf::from(format!("/tmp/wrk/{id}/output")),
            config: WorkspaceSpec {
                memory_mb: 512,
                timeout_secs: 60,
                cpu_percent: 100,
                max_pids: 64,
                network_mode: "none".into(),
                network_domains: vec![],
                seccomp: "standard".into(),
                idle_timeout_secs: 0,
                flavors: vec![],
            },
            git_repo: None,
            git_ref: None,
            label: None,
            domains: Vec::new(),
            last_exec_at: None,
            paused_at: None,
            auto_snapshot: None,
        }
    }

    #[tokio::test]
    async fn insert_and_get() {
        let store = InMemoryWorkspaceStore::new();
        store.insert(sample("wrk_a")).await.unwrap();
        let fetched = store.get("wrk_a").await.unwrap();
        assert_eq!(fetched.id, "wrk_a");
    }

    #[tokio::test]
    async fn duplicate_insert_is_conflict() {
        let store = InMemoryWorkspaceStore::new();
        store.insert(sample("wrk_a")).await.unwrap();
        let err = store.insert(sample("wrk_a")).await.unwrap_err();
        assert!(matches!(err, CtlError::Conflict(_)));
    }

    #[tokio::test]
    async fn soft_delete_hides_from_get_and_list() {
        let store = InMemoryWorkspaceStore::new();
        store.insert(sample("wrk_a")).await.unwrap();
        store.insert(sample("wrk_b")).await.unwrap();
        store.mark_deleted("wrk_a").await.unwrap();

        assert!(store.get("wrk_a").await.is_none());
        assert!(store.get("wrk_b").await.is_some());
        let (rows, total) = store.list(None, 100, 0, None).await;
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "wrk_b");
    }

    #[tokio::test]
    async fn list_pagination_is_stable() {
        let store = InMemoryWorkspaceStore::new();
        for i in 0..5 {
            let mut ws = sample(&format!("wrk_{i}"));
            ws.created_at = OffsetDateTime::now_utc() + time::Duration::seconds(i as i64);
            store.insert(ws).await.unwrap();
        }
        let (rows, total) = store.list(None, 2, 1, None).await;
        assert_eq!(total, 5);
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn list_q_filters_on_id_label_and_repo() {
        let store = InMemoryWorkspaceStore::new();
        let mut alpha = sample("wrk_alpha");
        alpha.label = Some("review-bot".into());
        alpha.git_repo = Some("https://github.com/org/alpha".into());
        store.insert(alpha).await.unwrap();

        let mut beta = sample("wrk_beta");
        beta.label = Some("ingest".into());
        beta.git_repo = Some("https://github.com/org/beta".into());
        store.insert(beta).await.unwrap();

        // id match
        let (rows, total) = store.list(None, 100, 0, Some("alpha")).await;
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "wrk_alpha");

        // label match (case-insensitive)
        let (rows, total) = store.list(None, 100, 0, Some("REVIEW")).await;
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "wrk_alpha");

        // git_repo match
        let (rows, total) = store.list(None, 100, 0, Some("org/beta")).await;
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "wrk_beta");

        // no match
        let (_, total) = store.list(None, 100, 0, Some("nomatch")).await;
        assert_eq!(total, 0);

        // empty / whitespace q = no filter
        let (_, total) = store.list(None, 100, 0, Some("   ")).await;
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn idle_pause_resume_cycle() {
        let store = InMemoryWorkspaceStore::new();
        store.insert(sample("wrk_a")).await.unwrap();

        store.touch("wrk_a").await;
        let w = store.get("wrk_a").await.unwrap();
        assert!(w.last_exec_at.is_some());
        assert!(w.paused_at.is_none());

        store.mark_paused("wrk_a", "snap_auto_1").await;
        let w = store.get("wrk_a").await.unwrap();
        assert!(w.paused_at.is_some());
        assert_eq!(w.auto_snapshot.as_deref(), Some("snap_auto_1"));

        let snap = store.mark_resumed("wrk_a").await;
        assert_eq!(snap.as_deref(), Some("snap_auto_1"));
        let w = store.get("wrk_a").await.unwrap();
        assert!(w.paused_at.is_none());
        assert!(w.auto_snapshot.is_none());
    }

    #[tokio::test]
    async fn set_label_updates_live_row_and_clears_to_none() {
        let store = InMemoryWorkspaceStore::new();
        store.insert(sample("wrk_a")).await.unwrap();

        let updated = store.set_label("wrk_a", Some("quiet-falcon-1234")).await.unwrap();
        assert_eq!(updated.label.as_deref(), Some("quiet-falcon-1234"));
        let fetched = store.get("wrk_a").await.unwrap();
        assert_eq!(fetched.label.as_deref(), Some("quiet-falcon-1234"));

        let cleared = store.set_label("wrk_a", None).await.unwrap();
        assert!(cleared.label.is_none());
    }

    #[tokio::test]
    async fn list_admin_sees_every_tenants_rows() {
        let store = InMemoryWorkspaceStore::new();
        store.insert(sample_in("wrk_a", "acme")).await.unwrap();
        store.insert(sample_in("wrk_b", "other")).await.unwrap();
        store.insert(sample_in("wrk_c", "dev")).await.unwrap();

        let (rows, total) = store.list(None, 100, 0, None).await;
        assert_eq!(total, 3);
        assert_eq!(rows.len(), 3);
    }

    #[tokio::test]
    async fn list_operator_only_sees_own_tenant() {
        let store = InMemoryWorkspaceStore::new();
        store.insert(sample_in("wrk_a", "acme")).await.unwrap();
        store.insert(sample_in("wrk_b", "other")).await.unwrap();
        store.insert(sample_in("wrk_c", "acme")).await.unwrap();

        let (rows, total) = store.list(Some("acme"), 100, 0, None).await;
        assert_eq!(total, 2);
        for r in &rows {
            assert_eq!(r.tenant_id, "acme");
        }
    }

    #[tokio::test]
    async fn list_tenant_filter_composes_with_q() {
        let store = InMemoryWorkspaceStore::new();
        let mut a = sample_in("wrk_a", "acme");
        a.label = Some("review".into());
        store.insert(a).await.unwrap();

        let mut b = sample_in("wrk_b", "other");
        b.label = Some("review".into());
        store.insert(b).await.unwrap();

        // Operator scoped to "acme" + q=review hits only acme's row.
        let (rows, total) = store.list(Some("acme"), 100, 0, Some("review")).await;
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "wrk_a");
    }

    #[tokio::test]
    async fn set_label_on_deleted_or_missing_returns_none() {
        let store = InMemoryWorkspaceStore::new();
        store.insert(sample("wrk_a")).await.unwrap();
        store.mark_deleted("wrk_a").await.unwrap();

        assert!(store.set_label("wrk_a", Some("x")).await.is_none());
        assert!(store.set_label("wrk_missing", Some("x")).await.is_none());
    }
}
