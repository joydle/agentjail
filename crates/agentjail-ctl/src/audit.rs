//! Audit log: bounded in-memory ring buffer of proxy requests.

use std::collections::VecDeque;
use std::sync::RwLock;

use agentjail_phantom::{AuditEntry, AuditSink};
use serde::Serialize;
use time::OffsetDateTime;

/// A single audit row, ready to be rendered in the UI.
#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    /// Row id (monotonic, server-local).
    pub id: u64,
    /// When the proxy handled the request.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Session the phantom token belonged to. Empty if rejected pre-auth.
    pub session_id: String,
    /// Service name (`openai`, `anthropic`, ...). Empty if unknown.
    pub service: String,
    /// HTTP method.
    pub method: String,
    /// Path (without the `/v1/<service>` prefix).
    pub path: String,
    /// Status code returned to the caller.
    pub status: u16,
    /// Reason for rejection, if any.
    pub reject_reason: Option<String>,
    /// Upstream latency in ms (if the upstream was contacted).
    pub upstream_ms: Option<u64>,
}

/// Bounded append-only audit store.
#[async_trait::async_trait]
pub trait AuditStore: Send + Sync + 'static {
    /// Append a row.
    async fn push(&self, row: AuditRow);
    /// List most-recent rows, newest first.
    async fn recent(&self, limit: usize) -> Vec<AuditRow>;
    /// Total number of rows ever recorded (may exceed retained rows).
    async fn total(&self) -> u64;
}

/// In-memory ring buffer. Default capacity 10 000 rows.
pub struct InMemoryAuditStore {
    inner: RwLock<Inner>,
    capacity: usize,
}

struct Inner {
    rows: VecDeque<AuditRow>,
    next_id: u64,
}

impl InMemoryAuditStore {
    /// Create with default capacity of 10 000 rows.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(10_000)
    }

    /// Create with custom capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                rows: VecDeque::with_capacity(capacity.min(1024)),
                next_id: 0,
            }),
            capacity,
        }
    }
}

impl Default for InMemoryAuditStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AuditStore for InMemoryAuditStore {
    async fn push(&self, row: AuditRow) {
        if let Ok(mut g) = self.inner.write() {
            let mut row = row;
            row.id = g.next_id;
            g.next_id = g.next_id.wrapping_add(1);
            if g.rows.len() >= self.capacity {
                g.rows.pop_front();
            }
            g.rows.push_back(row);
        }
    }

    async fn recent(&self, limit: usize) -> Vec<AuditRow> {
        let Ok(g) = self.inner.read() else {
            return Vec::new();
        };
        g.rows.iter().rev().take(limit).cloned().collect()
    }

    async fn total(&self) -> u64 {
        self.inner.read().map(|g| g.next_id).unwrap_or(0)
    }
}

/// Adapter: plug an `AuditStore` into the phantom proxy as its `AuditSink`.
pub struct AuditStoreSink<S: AuditStore> {
    pub(crate) store: std::sync::Arc<S>,
}

impl<S: AuditStore> AuditStoreSink<S> {
    /// Wrap a store as an `AuditSink`.
    pub fn new(store: std::sync::Arc<S>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl<S: AuditStore> AuditSink for AuditStoreSink<S> {
    async fn record(&self, entry: AuditEntry) {
        let row = AuditRow {
            id: 0, // assigned by push()
            at: OffsetDateTime::now_utc(),
            session_id: entry.session_id,
            service: entry
                .service
                .map(|s| s.name().to_string())
                .unwrap_or_default(),
            method: entry.method,
            path: entry.path,
            status: entry.status,
            reject_reason: entry.reject_reason.map(str::to_string),
            upstream_ms: entry
                .upstream_latency
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        };
        self.store.push(row).await;
    }
}
