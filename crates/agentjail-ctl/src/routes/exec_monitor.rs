//! Shared execution plumbing for the HTTP handlers.
//!
//! `run_monitored` / `run_monitored_with` spawn a jail, tail its pipes
//! into a bounded buffer, periodically flush to `JailStore::tail` for
//! the live-jail UI, and sample cgroup stats every 500 ms.
//!
//! Split out of [`super::exec`] so that file can stay focused on the
//! HTTP contract; this module owns the in-flight exec state machine
//! and is reused by `exec_in_session`, `create_run`, `create_fork_run`,
//! and `exec_in_workspace`.

use std::sync::Arc;

use crate::jails::JailStore;
use crate::sampler;
use crate::workspaces::{ActiveCgroups, ActiveJailIps};

/// RAII hook that records two pieces of per-workspace live-exec state:
/// the cgroup path (for mid-flight freeze-before-snapshot) and the
/// jail-side IP (for the gateway's `VmPort` resolver).
///
/// Constructed by the caller with the two trackers + workspace id;
/// fields filled in by [`run_monitored_with`] once the handle is up;
/// both cleaned up on drop so the trackers never hold stale entries.
pub(crate) struct ExecRegistration {
    cgroups: Arc<ActiveCgroups>,
    ips: Arc<ActiveJailIps>,
    workspace_id: String,
    set_cgroup: bool,
    set_ip: bool,
}

impl ExecRegistration {
    pub(crate) fn new(
        cgroups: Arc<ActiveCgroups>,
        ips: Arc<ActiveJailIps>,
        workspace_id: String,
    ) -> Self {
        Self { cgroups, ips, workspace_id, set_cgroup: false, set_ip: false }
    }

    fn set_cgroup(&mut self, path: std::path::PathBuf) {
        self.cgroups.insert(&self.workspace_id, path);
        self.set_cgroup = true;
    }

    fn set_ip(&mut self, ip: std::net::Ipv4Addr) {
        self.ips.insert(&self.workspace_id, ip);
        self.set_ip = true;
    }
}

impl Drop for ExecRegistration {
    fn drop(&mut self) {
        if self.set_cgroup { self.cgroups.remove(&self.workspace_id); }
        if self.set_ip     { self.ips.remove(&self.workspace_id); }
    }
}

/// Spawn + live-monitor + wait. Runs three cooperating tasks:
///   1. The jail process itself.
///   2. A cgroup sampler updating `memory_peak_bytes`, `cpu_usage_usec`,
///      and `io_*` every 500ms.
///   3. A stdout/stderr tailer that reads lines into a capped buffer
///      and flushes to `JailStore::tail` every 500ms so the Jails page
///      renders a live `tail -f` of any running jail, not just SSE
///      ones.
pub(super) async fn run_monitored(
    jails: &Arc<dyn JailStore>,
    rec_id: i64,
    jail: &agentjail::Jail,
    cmd: &str,
    args: &[&str],
) -> agentjail::Result<agentjail::Output> {
    run_monitored_with(jails, rec_id, jail, cmd, args, None).await
}

/// Variant of [`run_monitored`] that publishes live-exec state
/// (cgroup path + jail IP) to the supplied [`ExecRegistration`]. Used
/// by workspace execs so the snapshot route can freeze the running
/// jail and the gateway can route `VmPort` domains to it.
pub(super) async fn run_monitored_with(
    jails: &Arc<dyn JailStore>,
    rec_id: i64,
    jail: &agentjail::Jail,
    cmd: &str,
    args: &[&str],
    mut registration: Option<ExecRegistration>,
) -> agentjail::Result<agentjail::Output> {
    use std::sync::Mutex;
    let mut handle = jail.spawn(cmd, args)?;

    // Publish live-exec state: cgroup path for freeze-before-snapshot,
    // jail IP for the gateway's VmPort resolver. Both are optional —
    // `None` cgroup means no cgroup was set up (unusual), `None` ip
    // means the jail didn't use Allowlist network (normal).
    if let Some(reg) = registration.as_mut() {
        if let Some(p) = handle.cgroup_path() {
            reg.set_cgroup(p);
        }
        if let Some(ip) = handle.jail_ip() {
            reg.set_ip(ip);
        }
    }

    // Stats sampler (cgroup)
    let stats_task = handle.cgroup_path().map(|p| {
        let js = jails.clone();
        sampler::spawn(p, std::time::Duration::from_millis(500), move |s| {
            let js = js.clone();
            tokio::spawn(async move { js.sample_stats(rec_id, &s).await; });
        })
    });

    // Buffered stdout/stderr with periodic DB flush.
    let buf_stdout = Arc::new(Mutex::new(String::new()));
    let buf_stderr = Arc::new(Mutex::new(String::new()));

    // Flush ticker → JailStore::tail.
    let flush_task = {
        let js = jails.clone();
        let o  = buf_stdout.clone();
        let e  = buf_stderr.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await; // skip immediate
            loop {
                tick.tick().await;
                let (so, se) = {
                    let so = o.lock().map(|g| g.clone()).unwrap_or_default();
                    let se = e.lock().map(|g| g.clone()).unwrap_or_default();
                    (so, se)
                };
                js.tail(rec_id, &so, &se).await;
            }
        })
    };

    // Drain stdout/stderr in-process. We need to read them here because
    // handle.wait() only collects them at the end. Use tokio::select
    // over both pipes + the wait future; aggregate into the shared
    // buffers.
    let mut stdout_done = false;
    let mut stderr_done = false;
    while !stdout_done || !stderr_done {
        tokio::select! {
            biased;
            line = handle.stdout.read_line(), if !stdout_done => {
                match line {
                    Some(l) => { if let Ok(mut b) = buf_stdout.lock() { push_capped(&mut b, &l); } }
                    None    => { stdout_done = true; }
                }
            }
            line = handle.stderr.read_line(), if !stderr_done => {
                match line {
                    Some(l) => { if let Ok(mut b) = buf_stderr.lock() { push_capped(&mut b, &l); } }
                    None    => { stderr_done = true; }
                }
            }
        }
    }

    let out = handle.wait().await;
    if let Some(h) = stats_task { h.abort(); }
    flush_task.abort();

    // Push the final buffer so the DB reflects the full captured output
    // even when no one was tailing.
    let (so, se) = (
        buf_stdout.lock().map(|g| g.clone()).unwrap_or_default(),
        buf_stderr.lock().map(|g| g.clone()).unwrap_or_default(),
    );
    jails.tail(rec_id, &so, &se).await;

    // Merge captured bytes into the Output (handle.wait() returns empty
    // stdout/stderr because we drained the pipes line-by-line).
    out.map(|mut o| {
        if o.stdout.is_empty() && !so.is_empty() { o.stdout = so.into_bytes(); }
        if o.stderr.is_empty() && !se.is_empty() { o.stderr = se.into_bytes(); }
        o
    })
}

const OUTPUT_CAP_BYTES: usize = 16 * 1024;

fn push_capped(buf: &mut String, line: &str) {
    if buf.len() + line.len() > OUTPUT_CAP_BYTES {
        let take = OUTPUT_CAP_BYTES.saturating_sub(buf.len());
        buf.push_str(&line[..take.min(line.len())]);
        if !buf.ends_with("…") { buf.push('…'); }
        return;
    }
    buf.push_str(line);
}
