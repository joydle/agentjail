//! Jail execution configuration.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Controls how the `/v1/sessions/:id/exec` and `/v1/runs` endpoints
/// configure jails. When `None` in `ControlPlaneConfig`, those endpoints
/// return 501.
#[derive(Debug, Clone)]
pub struct ExecConfig {
    /// Default memory limit in MB for exec calls.
    pub default_memory_mb: u64,
    /// Default timeout in seconds.
    pub default_timeout_secs: u64,
    /// Maximum concurrent jail executions.
    pub max_concurrent: usize,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            default_memory_mb: 512,
            default_timeout_secs: 300,
            max_concurrent: 16,
        }
    }
}

/// Tracks active and total exec counts.
#[derive(Default)]
pub struct ExecMetrics {
    active: AtomicU64,
    total: AtomicU64,
}

impl ExecMetrics {
    /// New metrics tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment active count. Returns guard that decrements on drop.
    pub fn start(&self) -> ExecGuard<'_> {
        self.active.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
        ExecGuard(self)
    }

    /// Like [`start`], but returns a `'static` guard — suitable for handing
    /// into spawned tasks or SSE streams that outlive the request handler.
    pub fn start_owned(self: Arc<Self>) -> ExecOwnedGuard {
        self.active.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
        ExecOwnedGuard(self)
    }

    /// Current active executions.
    pub fn active(&self) -> u64 {
        self.active.load(Ordering::Relaxed)
    }

    /// Total executions since boot.
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
}

/// RAII guard that decrements active count on drop.
pub struct ExecGuard<'a>(&'a ExecMetrics);

impl Drop for ExecGuard<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// `'static` guard — decrements on drop, owns its Arc.
pub struct ExecOwnedGuard(Arc<ExecMetrics>);

impl Drop for ExecOwnedGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}
