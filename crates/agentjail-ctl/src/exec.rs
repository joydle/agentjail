//! Jail execution configuration.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Controls how the `/v1/sessions/:id/exec` and `/v1/runs` endpoints
/// configure jails. When `None` in `ControlPlaneConfig`, those endpoints
/// return 501.
#[derive(Debug, Clone)]
pub struct ExecConfig {
    /// Default memory limit in MB for exec calls.
    pub default_memory_mb: u64,
    /// Default timeout in seconds.
    pub default_timeout_secs: u64,
    /// Maximum concurrent jail executions (global). Every jail counts,
    /// including each parent + child of a `/v1/runs/fork` batch.
    pub max_concurrent: usize,
    /// Maximum concurrent jails per tenant. `0` = only the global cap
    /// applies.
    pub max_concurrent_per_tenant: usize,
    /// Lower bound for `memory_mb`. Requests below it (including `0`,
    /// which the engine treats as "no memory cgroup") are raised to it.
    pub min_memory_mb: u64,
    /// Upper bound for `memory_mb`.
    pub max_memory_mb: u64,
    /// Run jails in a user namespace. Default `true`. Root embedders are
    /// mapped to an unprivileged uid by the engine; turn this off only
    /// on hosts that cannot create user namespaces.
    pub user_namespace: bool,
    /// Apply landlock filesystem rules. Default `true`; the engine fails
    /// closed when the kernel lacks landlock, so set `false` explicitly
    /// to accept the weaker posture on such hosts.
    pub landlock: bool,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            default_memory_mb: 512,
            default_timeout_secs: 300,
            max_concurrent: 16,
            max_concurrent_per_tenant: 0,
            min_memory_mb: 64,
            max_memory_mb: 8192,
            user_namespace: true,
            landlock: true,
        }
    }
}

impl ExecConfig {
    /// Resolve a requested memory limit: fall back to `default`, then
    /// clamp to `min_memory_mb..=max_memory_mb` so `0` can never mean
    /// "unlimited".
    #[must_use]
    pub fn clamp_memory(&self, requested: Option<u64>, default: u64) -> u64 {
        let lo = self.min_memory_mb.max(1);
        let hi = self.max_memory_mb.max(lo);
        requested.unwrap_or(default).clamp(lo, hi)
    }
}

/// Per-tenant concurrent-jail counter. A cheap quota layer on top of
/// the global semaphore so one tenant can't take every slot.
#[derive(Default)]
pub(crate) struct TenantLimiter {
    max_per_tenant: usize,
    inner: Mutex<HashMap<String, usize>>,
}

impl TenantLimiter {
    pub(crate) fn new(max_per_tenant: usize) -> Self {
        Self {
            max_per_tenant,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Reserve `n` slots for `tenant`. `None` when that would exceed the
    /// per-tenant cap. Always succeeds when the cap is `0`.
    pub(crate) fn try_acquire(self: &Arc<Self>, tenant: &str, n: usize) -> Option<TenantPermit> {
        if self.max_per_tenant > 0 {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let cur = g.entry(tenant.to_string()).or_insert(0);
            if *cur + n > self.max_per_tenant {
                return None;
            }
            *cur += n;
        }
        Some(TenantPermit {
            limiter: self.clone(),
            tenant: tenant.to_string(),
            n,
        })
    }
}

/// Releases its slots on drop.
pub(crate) struct TenantPermit {
    limiter: Arc<TenantLimiter>,
    tenant: String,
    n: usize,
}

impl Drop for TenantPermit {
    fn drop(&mut self) {
        if self.limiter.max_per_tenant == 0 {
            return;
        }
        let mut g = self.limiter.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cur) = g.get_mut(&self.tenant) {
            *cur = cur.saturating_sub(self.n);
            if *cur == 0 {
                g.remove(&self.tenant);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_zero_is_raised_to_minimum() {
        let c = ExecConfig::default();
        assert_eq!(c.clamp_memory(Some(0), 512), 64);
        assert_eq!(c.clamp_memory(Some(1), 512), 64);
        assert_eq!(c.clamp_memory(None, 512), 512);
        assert_eq!(c.clamp_memory(Some(999_999), 512), 8192);
    }

    #[test]
    fn tenant_limiter_caps_per_tenant_and_releases() {
        let l = Arc::new(TenantLimiter::new(2));
        let a = l.try_acquire("acme", 2).unwrap();
        assert!(l.try_acquire("acme", 1).is_none());
        // Other tenants are unaffected.
        let _b = l.try_acquire("other", 1).unwrap();
        drop(a);
        assert!(l.try_acquire("acme", 2).is_some());
    }

    #[test]
    fn tenant_limiter_zero_means_unlimited() {
        let l = Arc::new(TenantLimiter::new(0));
        let _p: Vec<_> = (0..100).map(|_| l.try_acquire("t", 5).unwrap()).collect();
    }
}
