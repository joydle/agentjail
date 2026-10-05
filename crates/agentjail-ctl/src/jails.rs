//! Ledger of jail runs (exec, run, fork, stream).
//!
//! One record per HTTP request that spawned a jail. The trait here defines
//! the store contract; `InMemoryJailStore` is the default, and
//! `db::PgJailStore` implements the same surface against Postgres.
//!
//! Stored output is capped at 16 KiB per stream to keep responses small.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const DEFAULT_CAPACITY: usize = 2_000;
/// Hard cap on stdout/stderr stored per row.
pub const OUTPUT_CAP: usize = 16 * 1024;

/// Kind of jail run.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JailKind {
    /// `/v1/runs`
    Run,
    /// `/v1/sessions/:id/exec`
    Exec,
    /// `/v1/runs/fork` (one row per parent + child)
    Fork,
    /// `/v1/runs/stream`
    Stream,
    /// `/v1/workspaces/:id/exec` — jail backed by a persistent workspace.
    Workspace,
}

impl JailKind {
    /// Wire form used in SQL column.
    pub fn as_str(&self) -> &'static str {
        match self {
            JailKind::Run => "run",
            JailKind::Exec => "exec",
            JailKind::Fork => "fork",
            JailKind::Stream => "stream",
            JailKind::Workspace => "workspace",
        }
    }

    /// Parse from the DB column.
    pub fn from_str_or_run(s: &str) -> Self {
        match s {
            "exec"      => JailKind::Exec,
            "fork"      => JailKind::Fork,
            "stream"    => JailKind::Stream,
            "workspace" => JailKind::Workspace,
            _           => JailKind::Run,
        }
    }
}

/// Current status of a jail record.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JailStatus {
    /// Jail is spawned and the process is still being waited on.
    Running,
    /// Process exited normally; `exit_code` + timing are populated.
    Completed,
    /// Jail failed to spawn or was killed by the engine (OOM, timeout,
    /// seccomp violation); `error` carries the reason.
    Error,
}

impl JailStatus {
    /// Wire form used in SQL column.
    pub fn as_str(&self) -> &'static str {
        match self {
            JailStatus::Running   => "running",
            JailStatus::Completed => "completed",
            JailStatus::Error     => "error",
        }
    }

    /// Parse from the DB column.
    pub fn from_str_or_error(s: &str) -> Self {
        match s {
            "running"   => JailStatus::Running,
            "completed" => JailStatus::Completed,
            _           => JailStatus::Error,
        }
    }
}

/// Snapshot of the jail config that was applied at start time.
///
/// Captured once per run so the Jails ledger can answer "what did this
/// run with?" — the fields mirror the knobs that drive `JailConfig`.
/// Everything here is safe to surface over HTTP: no secrets, no
/// arbitrary code. Populated by the route handlers via
/// [`JailStore::attach_config`] right after [`JailStore::start`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JailConfigSnapshot {
    /// `"none"` | `"loopback"` | `"allowlist"`.
    pub network_mode: String,
    /// Allowlist entries when `network_mode == "allowlist"`. Empty
    /// otherwise (omitted from JSON to keep the default row tiny).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_domains: Vec<String>,
    /// `"standard"` | `"strict"`.
    pub seccomp: String,
    /// Effective memory cap in MB.
    pub memory_mb: u64,
    /// Exec timeout in seconds.
    pub timeout_secs: u64,
    /// CPU quota (100 = one core).
    pub cpu_percent: u64,
    /// PID cap.
    pub max_pids: u64,
    /// Primary git repo URL when a clone happened before the jail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_repo: Option<String>,
    /// Git ref (branch/tag/commit) paired with `git_repo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

/// A single jail-run row.
#[derive(Debug, Clone, Serialize)]
pub struct JailRecord {
    /// Monotonic id assigned by the store.
    pub id: i64,
    /// Tenant the invoking request belongs to. Stamped at `start()`
    /// time; route-layer filters keep ops from reading rows outside
    /// their own tenant.
    pub tenant_id: String,
    /// What invocation produced this row.
    pub kind: JailKind,
    /// When the jail started.
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    /// When it finished (None while still running).
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    /// Lifecycle state.
    pub status: JailStatus,

    /// Optional — attached session id (for `exec` and `stream` modes).
    pub session_id: Option<String>,

    /// Short label: language for `run`/`stream`/`fork`, or the invoked
    /// command for `exec`.
    pub label: String,
    /// For fork children — the id of the parent jail row they were cloned
    /// from. `None` for everything else.
    pub parent_id: Option<i64>,

    // ─── set on finish ─────────────────────────────────────────────────
    /// Exit code of the jailed process.
    pub exit_code:   Option<i32>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Whether the jail was killed by the timeout.
    pub timed_out:   Option<bool>,
    /// Whether the jail was killed by OOM.
    pub oom_killed:  Option<bool>,
    /// Peak memory used by the cgroup.
    pub memory_peak_bytes: Option<u64>,
    /// Current memory at the last sample (live).
    pub memory_current_bytes: Option<u64>,
    /// Total CPU time in microseconds.
    pub cpu_usage_usec:    Option<u64>,
    /// Bytes read from disk inside the jail.
    pub io_read_bytes:     Option<u64>,
    /// Bytes written to disk inside the jail.
    pub io_write_bytes:    Option<u64>,
    /// Processes alive at the last sample.
    pub pids_current:      Option<u64>,
    /// Captured stdout (truncated).
    pub stdout: Option<String>,
    /// Captured stderr (truncated).
    pub stderr: Option<String>,
    /// Error message when the jail failed to spawn.
    pub error:  Option<String>,

    /// What the jail actually ran with (network policy, seccomp level,
    /// limits, optional git seed). Populated after `start()` via
    /// [`JailStore::attach_config`]. `None` for legacy rows predating
    /// this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<JailConfigSnapshot>,
}

impl JailRecord {
    /// Build a `Running` row with the passed invocation context.
    pub fn new_running(
        id: i64,
        tenant_id: String,
        kind: JailKind,
        label: String,
        session_id: Option<String>,
        parent_id: Option<i64>,
    ) -> Self {
        Self {
            id,
            tenant_id,
            kind,
            started_at: OffsetDateTime::now_utc(),
            ended_at: None,
            status: JailStatus::Running,
            session_id,
            label,
            parent_id,
            exit_code: None,
            duration_ms: None,
            timed_out: None,
            oom_killed: None,
            memory_peak_bytes: None,
            memory_current_bytes: None,
            cpu_usage_usec: None,
            io_read_bytes: None,
            io_write_bytes: None,
            pids_current: None,
            stdout: None,
            stderr: None,
            error: None,
            config: None,
        }
    }
}

/// Bounded store contract. Implementations must be cheap to clone via Arc.
#[async_trait]
pub trait JailStore: Send + Sync + 'static {
    /// Insert a `Running` record and return its id. `tenant_id` is
    /// stamped onto the row; route handlers pass the caller's
    /// [`crate::tenant::TenantScope::tenant`].
    async fn start(
        &self,
        tenant_id: String,
        kind: JailKind,
        label: String,
        session_id: Option<String>,
        parent_id: Option<i64>,
    ) -> i64;
    /// Mark a record as `Completed` and populate it with captured output.
    async fn finish(&self, id: i64, output: &agentjail::Output);
    /// Update live resource stats for a running record (mid-flight sampler).
    /// Default impl is a no-op — Postgres backend overrides this to persist
    /// the sample and the in-memory backend updates the row in place.
    async fn sample_stats(&self, _id: i64, _stats: &agentjail::ResourceStats) {}
    /// Mark a record as `Error` with a short message.
    async fn error(&self, id: i64, message: String);
    /// Return the most-recent rows, newest first, optionally filtered.
    async fn recent(&self, limit: usize, status: Option<JailStatus>)
        -> (Vec<JailRecord>, u64);
    /// Paged, filtered, searched. `q` matches label / session_id / error
    /// (case-insensitive substring). Returns `(rows, total_after_filter)`.
    async fn page(&self, q: JailQuery) -> (Vec<JailRecord>, u64);
    /// Append captured output so far. Called by the unified exec helper
    /// every ~500ms so the Jails detail reads a live tail.
    async fn tail(&self, _id: i64, _stdout: &str, _stderr: &str) {}
    /// Attach the config snapshot the jail ran with. Called by each
    /// route handler after `start()`. Default is a no-op so external
    /// implementations aren't forced to persist the new column; the
    /// bundled in-memory + Postgres impls override.
    async fn attach_config(&self, _id: i64, _config: JailConfigSnapshot) {}
    /// Fetch a single record by id.
    async fn get(&self, id: i64) -> Option<JailRecord>;
}

/// Paged query over the jail ledger.
#[derive(Debug, Clone, Default)]
pub struct JailQuery {
    /// Tenant filter. `None` = cross-tenant (admin); `Some(id)` keeps
    /// only rows stamped with that tenant.
    pub tenant: Option<String>,
    /// Max rows to return (1..=500).
    pub limit: usize,
    /// Skip this many rows from the head (newest).
    pub offset: usize,
    /// Filter by status.
    pub status: Option<JailStatus>,
    /// Filter by kind.
    pub kind: Option<JailKind>,
    /// Case-insensitive substring match on label / session_id / error.
    pub q: Option<String>,
}

/// In-memory ring buffer implementation (default when no DATABASE_URL).
pub struct InMemoryJailStore {
    inner: Mutex<Inner>,
    capacity: usize,
}

struct Inner {
    rows: VecDeque<JailRecord>,
    next_id: i64,
}

impl InMemoryJailStore {
    /// Create with the default 2000-row capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Create with a custom capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                rows: VecDeque::with_capacity(capacity.min(1024)),
                next_id: 0,
            }),
            capacity,
        }
    }
}

impl Default for InMemoryJailStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl JailStore for InMemoryJailStore {
    async fn start(
        &self,
        tenant_id: String,
        kind: JailKind,
        label: String,
        session_id: Option<String>,
        parent_id: Option<i64>,
    ) -> i64 {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        let rec = JailRecord::new_running(id, tenant_id, kind, label, session_id, parent_id);
        if g.rows.len() >= self.capacity {
            g.rows.pop_front();
        }
        g.rows.push_back(rec);
        id
    }

    async fn finish(&self, id: i64, output: &agentjail::Output) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(rec) = g.rows.iter_mut().find(|r| r.id == id) {
            rec.ended_at = Some(OffsetDateTime::now_utc());
            rec.status   = JailStatus::Completed;
            rec.exit_code   = Some(output.exit_code);
            rec.duration_ms = Some(u64::try_from(output.duration.as_millis()).unwrap_or(u64::MAX));
            rec.timed_out   = Some(output.timed_out);
            rec.oom_killed  = Some(output.oom_killed);
            if let Some(s) = output.stats.as_ref() {
                rec.memory_peak_bytes    = Some(s.memory_peak_bytes);
                rec.memory_current_bytes = Some(s.memory_current_bytes);
                rec.cpu_usage_usec       = Some(s.cpu_usage_usec);
                rec.io_read_bytes        = Some(s.io_read_bytes);
                rec.io_write_bytes       = Some(s.io_write_bytes);
                rec.pids_current         = Some(s.pids_current);
            }
            rec.stdout = Some(truncate(&String::from_utf8_lossy(&output.stdout), OUTPUT_CAP));
            rec.stderr = Some(truncate(&String::from_utf8_lossy(&output.stderr), OUTPUT_CAP));
        }
    }

    async fn sample_stats(&self, id: i64, stats: &agentjail::ResourceStats) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(rec) = g.rows.iter_mut().find(|r| r.id == id) {
            rec.memory_peak_bytes    = Some(stats.memory_peak_bytes);
            rec.memory_current_bytes = Some(stats.memory_current_bytes);
            rec.cpu_usage_usec       = Some(stats.cpu_usage_usec);
            rec.io_read_bytes        = Some(stats.io_read_bytes);
            rec.io_write_bytes       = Some(stats.io_write_bytes);
            rec.pids_current         = Some(stats.pids_current);
        }
    }

    async fn attach_config(&self, id: i64, config: JailConfigSnapshot) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(rec) = g.rows.iter_mut().find(|r| r.id == id) {
            rec.config = Some(config);
        }
    }

    async fn error(&self, id: i64, message: String) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(rec) = g.rows.iter_mut().find(|r| r.id == id) {
            rec.ended_at = Some(OffsetDateTime::now_utc());
            rec.status   = JailStatus::Error;
            rec.error    = Some(message);
        }
    }

    async fn recent(&self, limit: usize, status: Option<JailStatus>)
        -> (Vec<JailRecord>, u64)
    {
        let g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let total = g.next_id as u64;
        let rows = g.rows.iter().rev()
            .filter(|r| status.is_none_or(|s| r.status == s))
            .take(limit)
            .cloned()
            .collect();
        (rows, total)
    }

    async fn page(&self, q: JailQuery) -> (Vec<JailRecord>, u64) {
        let g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let needle = q.q.as_deref().map(|s| s.to_lowercase());
        let tenant = q.tenant.as_deref();
        let matches = |r: &JailRecord| -> bool {
            if let Some(t) = tenant      { if r.tenant_id != t { return false; } }
            if let Some(s) = q.status    { if r.status != s { return false; } }
            if let Some(k) = q.kind      { if !matches_kind(r.kind, k) { return false; } }
            if let Some(n) = needle.as_deref() {
                let hay_lbl = r.label.to_lowercase();
                let hay_sid = r.session_id.as_deref().unwrap_or("").to_lowercase();
                let hay_err = r.error.as_deref().unwrap_or("").to_lowercase();
                if !hay_lbl.contains(n) && !hay_sid.contains(n) && !hay_err.contains(n) {
                    return false;
                }
            }
            true
        };
        let filtered: Vec<&JailRecord> = g.rows.iter().rev().filter(|r| matches(r)).collect();
        let total = filtered.len() as u64;
        let rows: Vec<JailRecord> = filtered.into_iter()
            .skip(q.offset)
            .take(q.limit.clamp(1, 500))
            .cloned()
            .collect();
        (rows, total)
    }

    async fn tail(&self, id: i64, stdout: &str, stderr: &str) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(rec) = g.rows.iter_mut().find(|r| r.id == id) {
            rec.stdout = Some(truncate(stdout, OUTPUT_CAP));
            rec.stderr = Some(truncate(stderr, OUTPUT_CAP));
        }
    }

    async fn get(&self, id: i64) -> Option<JailRecord> {
        let g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.rows.iter().find(|r| r.id == id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn page_filters_by_tenant() {
        let store = InMemoryJailStore::new();
        let _ = store.start("acme".into(),  JailKind::Run,  "a".into(), None, None).await;
        let _ = store.start("other".into(), JailKind::Run,  "b".into(), None, None).await;
        let _ = store.start("acme".into(),  JailKind::Exec, "c".into(), None, None).await;

        let (rows, total) = store.page(JailQuery {
            tenant: Some("acme".into()),
            limit: 100, offset: 0,
            status: None, kind: None, q: None,
        }).await;
        assert_eq!(total, 2);
        for r in &rows {
            assert_eq!(r.tenant_id, "acme");
        }
    }

    #[tokio::test]
    async fn page_admin_sees_every_tenant() {
        let store = InMemoryJailStore::new();
        let _ = store.start("a".into(), JailKind::Run, "x".into(), None, None).await;
        let _ = store.start("b".into(), JailKind::Run, "y".into(), None, None).await;

        let (_, total) = store.page(JailQuery {
            tenant: None, limit: 100, offset: 0,
            status: None, kind: None, q: None,
        }).await;
        assert_eq!(total, 2);
    }
}

/// Truncate strings to a byte cap with a clear suffix.
pub(crate) fn truncate(s: &str, cap: usize) -> String {
    if s.len() <= cap { s.to_owned() }
    else { format!("{}… (truncated)", &s[..cap]) }
}

fn matches_kind(a: JailKind, b: JailKind) -> bool {
    a == b
}

/// Label format for workspace-kind jail rows: `workspace:<id>/<cmd>`.
///
/// The web UI's `RecentExecs` block and the jail-ledger substring
/// search both key off this prefix. Change here once, not at the call
/// site, so the Rust writer and the TS reader can't drift.
#[must_use]
pub fn workspace_exec_label(workspace_id: &str, cmd: &str) -> String {
    format!("workspace:{workspace_id}/{cmd}")
}
