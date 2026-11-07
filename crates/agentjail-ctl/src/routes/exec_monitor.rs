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
//!
//! Three invariants hold for every exec started here:
//!
//! 1. **Deadline.** The pipe drain *and* the final `wait()` are bounded
//!    by `start + timeout`. A jail that keeps its pipes open (or a
//!    grandchild that inherited them) is killed at the deadline and the
//!    result reports `timed_out = true`.
//! 2. **No leaked tasks.** The stats sampler and tail flusher are held
//!    in [`AbortOnDrop`] guards, so a cancelled request (client gone)
//!    stops them along with the jail.
//! 3. **Rows are always finalised.** The ledger row is wrapped in a
//!    [`JailRow`]; if the request is dropped before `finish`/`error`,
//!    the guard records the row as cancelled.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::{CtlError, Result};
use crate::jails::JailStore;
use crate::sampler;
use crate::workspaces::{ActiveCgroups, ActiveJailIps};

/// Upper bound on jail setup (fork + namespace / cgroup / veth
/// handshake). A wedged setup must not hang the request forever.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Extra time allowed for `wait()` (reap + stats collection) after the
/// deadline has passed and the process tree has been killed.
const WAIT_GRACE: Duration = Duration::from_secs(5);

/// Abort a background task when the guard goes out of scope. Dropping a
/// plain `JoinHandle` detaches the task, so a cancelled request would
/// leave its sampler / flusher loops running forever.
pub(crate) struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl AbortOnDrop {
    pub(crate) fn new(h: tokio::task::JoinHandle<()>) -> Self {
        Self(h)
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A started jail-ledger row that must be finalised exactly once.
///
/// Call [`Self::finish`] or [`Self::error`]. If the guard is dropped
/// without either (the handler future was cancelled because the client
/// disconnected), the row is marked as errored in a detached task so it
/// never stays `running` forever.
pub(crate) struct JailRow {
    jails: Arc<dyn JailStore>,
    id: i64,
    done: bool,
}

impl JailRow {
    pub(crate) fn new(jails: Arc<dyn JailStore>, id: i64) -> Self {
        Self {
            jails,
            id,
            done: false,
        }
    }

    pub(crate) fn id(&self) -> i64 {
        self.id
    }

    pub(crate) async fn finish(mut self, out: &agentjail::Output) {
        self.done = true;
        self.jails.finish(self.id, out).await;
    }

    pub(crate) async fn error(mut self, msg: String) {
        self.done = true;
        self.jails.error(self.id, msg).await;
    }
}

impl Drop for JailRow {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let jails = self.jails.clone();
        let id = self.id;
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                jails
                    .error(id, "cancelled: request aborted before completion".into())
                    .await;
            });
        }
    }
}

/// Build a jail and spawn the command on tokio's blocking pool.
/// `Jail::new` compiles the seccomp filter and probes the filesystem;
/// `Jail::spawn` forks and blocks on the child's setup handshake.
/// Neither belongs on an async worker thread. (The engine forks on its
/// own process-lifetime thread, so the jail doesn't depend on the
/// blocking-pool thread that called `spawn` staying alive.)
pub(super) async fn spawn_jail(
    config: agentjail::JailConfig,
    cmd: String,
    args: Vec<String>,
) -> Result<(agentjail::Jail, agentjail::JailHandle)> {
    let jail = blocking(move || agentjail::Jail::new(config).map_err(CtlError::from)).await?;
    spawn_on(jail, cmd, args).await
}

/// Spawn a command in an existing jail on the blocking pool.
pub(super) async fn spawn_on(
    jail: agentjail::Jail,
    cmd: String,
    args: Vec<String>,
) -> Result<(agentjail::Jail, agentjail::JailHandle)> {
    // On timeout the blocking task keeps running; when `spawn`
    // eventually returns, the result is dropped there (the receiver is
    // gone), and the handle's Drop kills and reaps the child.
    tokio::time::timeout(
        SPAWN_TIMEOUT,
        blocking(move || {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let h = jail.spawn(&cmd, &refs)?;
            Ok((jail, h))
        }),
    )
    .await
    .map_err(|_| {
        CtlError::Internal(format!(
            "jail spawn did not complete within {SPAWN_TIMEOUT:?}"
        ))
    })?
}

/// Run a blocking closure on tokio's blocking pool and flatten the join
/// error into a [`CtlError`].
pub(super) async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CtlError::Internal(format!("blocking task failed: {e}")))?
}

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
        Self {
            cgroups,
            ips,
            workspace_id,
            set_cgroup: false,
            set_ip: false,
        }
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
        if self.set_cgroup {
            self.cgroups.remove(&self.workspace_id);
        }
        if self.set_ip {
            self.ips.remove(&self.workspace_id);
        }
    }
}

/// Spawn + live-monitor + wait, then finalise `row`. See
/// [`run_monitored_with`].
pub(super) async fn run_monitored(
    jails: &Arc<dyn JailStore>,
    row: JailRow,
    jail: agentjail::Jail,
    cmd: &str,
    args: &[String],
    timeout: Duration,
) -> Result<agentjail::Output> {
    run_monitored_with(jails, row, jail, cmd, args, timeout, None).await
}

/// Spawn + live-monitor + wait. Runs three cooperating tasks:
///   1. The jail process itself.
///   2. A cgroup sampler updating `memory_peak_bytes`, `cpu_usage_usec`,
///      and `io_*` every 500ms.
///   3. A stdout/stderr tailer that reads lines into a capped buffer
///      and flushes to `JailStore::tail` every 500ms so the Jails page
///      renders a live `tail -f` of any running jail, not just SSE
///      ones.
///
/// The whole drain + wait is bounded by `spawn + timeout`; on expiry
/// the process tree is killed and the output reports `timed_out`.
/// `row` is finalised (finish or error) before returning.
///
/// Publishes live-exec state (cgroup path + jail IP) to the supplied
/// [`ExecRegistration`] so the snapshot route can freeze the running
/// jail and the gateway can route `VmPort` domains to it.
pub(super) async fn run_monitored_with(
    jails: &Arc<dyn JailStore>,
    row: JailRow,
    jail: agentjail::Jail,
    cmd: &str,
    args: &[String],
    timeout: Duration,
    mut registration: Option<ExecRegistration>,
) -> Result<agentjail::Output> {
    let rec_id = row.id();
    let (jail, mut handle) = match spawn_on(jail, cmd.to_string(), args.to_vec()).await {
        Ok(v) => v,
        Err(e) => {
            row.error(e.to_string()).await;
            return Err(e);
        }
    };
    let started = tokio::time::Instant::now();
    let deadline = started + timeout;

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
        AbortOnDrop::new(sampler::spawn(p, Duration::from_millis(500), move |s| {
            let js = js.clone();
            tokio::spawn(async move {
                js.sample_stats(rec_id, &s).await;
            });
        }))
    });

    // Buffered stdout/stderr with periodic DB flush.
    let buf_stdout = Arc::new(Mutex::new(String::new()));
    let buf_stderr = Arc::new(Mutex::new(String::new()));

    // Flush ticker → JailStore::tail.
    let flush_task = {
        let js = jails.clone();
        let o = buf_stdout.clone();
        let e = buf_stderr.clone();
        AbortOnDrop::new(tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
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
        }))
    };

    // Drain both pipes concurrently (each in its own future, so neither
    // read is ever cancelled mid-line), bounded by the deadline.
    let mut timed_out = false;
    {
        let out = &mut handle.stdout;
        let err = &mut handle.stderr;
        let drain_out = async {
            while let Some(l) = out.read_line().await {
                if let Ok(mut b) = buf_stdout.lock() {
                    push_capped(&mut b, &l);
                }
            }
        };
        let drain_err = async {
            while let Some(l) = err.read_line().await {
                if let Ok(mut b) = buf_stderr.lock() {
                    push_capped(&mut b, &l);
                }
            }
        };
        let drained = tokio::time::timeout_at(deadline, async {
            tokio::join!(drain_out, drain_err);
        })
        .await;
        if drained.is_err() {
            timed_out = true;
        }
    }
    if timed_out {
        handle.kill();
    }

    // `wait()` enforces the jail's own timeout too, but bound it here as
    // well: after a kill it should return promptly; if it doesn't (pipes
    // held open by an escaped process), give up and let the handle's
    // Drop kill + reap.
    let wait_deadline = std::cmp::max(deadline, tokio::time::Instant::now()) + WAIT_GRACE;
    let out = match tokio::time::timeout_at(wait_deadline, handle.wait()).await {
        Ok(r) => r.map_err(CtlError::from),
        Err(_) => {
            timed_out = true;
            Ok(agentjail::Output {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 137,
                duration: started.elapsed(),
                timed_out: true,
                oom_killed: false,
                stats: None,
            })
        }
    };
    drop(stats_task);
    drop(flush_task);
    drop(jail);

    // Push the final buffer so the DB reflects the full captured output
    // even when no one was tailing.
    let (so, se) = (
        buf_stdout.lock().map(|g| g.clone()).unwrap_or_default(),
        buf_stderr.lock().map(|g| g.clone()).unwrap_or_default(),
    );
    jails.tail(rec_id, &so, &se).await;

    // Merge captured bytes into the Output (handle.wait() returns empty
    // stdout/stderr because we drained the pipes line-by-line).
    let out = out.map(|mut o| {
        if o.stdout.is_empty() && !so.is_empty() {
            o.stdout = so.into_bytes();
        }
        if o.stderr.is_empty() && !se.is_empty() {
            o.stderr = se.into_bytes();
        }
        o.timed_out |= timed_out;
        o
    });
    match &out {
        Ok(o) => row.finish(o).await,
        Err(e) => row.error(e.to_string()).await,
    }
    out
}

const OUTPUT_CAP_BYTES: usize = 16 * 1024;

/// Largest index `<= i` that lies on a UTF-8 char boundary of `s`
/// (stable stand-in for `str::floor_char_boundary`).
fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut i = i;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub(super) fn push_capped(buf: &mut String, line: &str) {
    if buf.len() + line.len() > OUTPUT_CAP_BYTES {
        let take = OUTPUT_CAP_BYTES.saturating_sub(buf.len());
        let take = floor_char_boundary(line, take);
        buf.push_str(&line[..take]);
        if !buf.ends_with('…') {
            buf.push('…');
        }
        return;
    }
    buf.push_str(line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_capped_does_not_split_multibyte_char_at_cap() {
        // 16383 ASCII bytes + 'é' (2 bytes) straddles the 16 KiB cap:
        // slicing at byte 16384 would land inside 'é' and panic.
        let mut b = String::new();
        let line = format!("{}é\n", "a".repeat(OUTPUT_CAP_BYTES - 1));
        push_capped(&mut b, &line);
        assert!(b.starts_with(&"a".repeat(OUTPUT_CAP_BYTES - 1)));
        assert!(b.ends_with('…'));
        assert!(!b.contains('é'));
    }

    #[test]
    fn push_capped_keeps_whole_chars_when_partially_full() {
        let mut b = "x".repeat(OUTPUT_CAP_BYTES - 3);
        // Room for 3 bytes; '€' is 3 bytes, 'é' is 2 → only "€" fits.
        push_capped(&mut b, "€é€");
        assert!(b.ends_with("€…"), "got tail {:?}", &b[b.len() - 8..]);
    }

    #[test]
    fn push_capped_passes_through_under_cap() {
        let mut b = String::new();
        push_capped(&mut b, "héllo\n");
        assert_eq!(b, "héllo\n");
    }

    #[test]
    fn floor_char_boundary_steps_back() {
        let s = "aé";
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(floor_char_boundary(s, 1), 1);
        assert_eq!(floor_char_boundary(s, 99), s.len());
    }

    #[tokio::test]
    async fn abort_on_drop_stops_task() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        let g = AbortOnDrop::new(tokio::spawn(async move {
            let _tx = tx;
            std::future::pending::<()>().await;
        }));
        drop(g);
        // Sender is dropped only when the task is aborted.
        assert!(
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("task was not aborted")
                .is_none()
        );
    }

    #[tokio::test]
    async fn dropped_jail_row_is_finalised_as_error() {
        use crate::jails::{InMemoryJailStore, JailKind, JailStatus};
        let store: Arc<dyn JailStore> = Arc::new(InMemoryJailStore::new());
        let id = store
            .start("dev".into(), JailKind::Run, "x".into(), None, None)
            .await;
        drop(JailRow::new(store.clone(), id));
        for _ in 0..50 {
            if let Some(r) = store.get(id).await
                && !matches!(r.status, JailStatus::Running)
            {
                assert!(matches!(r.status, JailStatus::Error));
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("cancelled jail row stayed running");
    }
}
