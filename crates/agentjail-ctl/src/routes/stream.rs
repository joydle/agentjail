//! SSE streaming endpoint — `POST /v1/runs/stream`.
//!
//! Emits:
//!   event: started    data: {"pid": ...}
//!   event: stdout     data: <line>
//!   event: stderr     data: <line>
//!   event: stats      data: {"memory_peak_bytes":..,"cpu_usage_usec":..,...}
//!   event: completed  data: {"exit_code":..,"duration_ms":..,...}
//!
//! The response closes right after `completed` (or `error`).

use axum::Json;
use axum::extract::State;

use crate::error::{CtlError, Result};
use crate::jails::JailKind;
use crate::sampler;

use super::AppState;
use super::exec::{RunRequest, config_snapshot, jail_config, language_runtime};

pub(crate) async fn create_stream_run(
    State(state): State<AppState>,
    scope: crate::tenant::TenantScope,
    Json(req): Json<RunRequest>,
) -> Result<axum::response::sse::Sse<
    futures::stream::BoxStream<
        'static,
        std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
    >,
>> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures::StreamExt;

    let exec_cfg = state.exec_config.as_ref()
        .ok_or_else(|| CtlError::Internal("exec not enabled".into()))?;

    if req.code.len() > 1024 * 1024 {
        return Err(CtlError::BadRequest("code exceeds 1 MB".into()));
    }
    let timeout = req.timeout_secs.unwrap_or(exec_cfg.default_timeout_secs).clamp(1, 3600);
    let memory  = req.memory_mb.unwrap_or(exec_cfg.default_memory_mb).min(8192);

    let permit = state.exec_semaphore.clone().try_acquire_owned()
        .map_err(|_| CtlError::BadRequest("too many concurrent executions".into()))?;

    let (filename, cmd) = language_runtime(&req.language)?;

    let source_dir = tempfile::tempdir().map_err(CtlError::Io)?;
    let output_dir = tempfile::tempdir().map_err(CtlError::Io)?;
    std::fs::write(source_dir.path().join(filename), &req.code).map_err(CtlError::Io)?;

    let run_env = vec![("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into())];
    let config  = jail_config(
        source_dir.path(), output_dir.path(), memory, timeout, run_env, &req.options,
        /* source_rw */ false,
        Vec::new(),
    )?;

    let jail = agentjail::Jail::new(config)?;
    let mut handle = jail.spawn(cmd, &[&format!("/workspace/{filename}")])?;
    let pid: u32 = handle.pid().as_raw();

    let started_guard = state.exec_metrics.clone().start_owned();
    let stream_rec    = state.jails
        .start(scope.tenant.clone(), JailKind::Stream, req.language.clone(), None, None)
        .await;
    state.jails.attach_config(
        stream_rec,
        config_snapshot(&req.options, memory, timeout, req.git.as_ref()),
    ).await;
    let jails_store   = state.jails.clone();

    // Persist mid-flight stats so the Jails detail view stays live even
    // for clients that aren't consuming the SSE stream.
    let stats_sampler = handle.cgroup_path().map(|p| {
        let js = jails_store.clone();
        sampler::spawn(p, std::time::Duration::from_millis(500), move |s| {
            let js = js.clone();
            tokio::spawn(async move { js.sample_stats(stream_rec, &s).await; });
        })
    });

    // Separate sampler for SSE emission — same cadence, same source. The
    // SSE stream owns its own channel so backpressure is per-client.
    let (sse_tx, mut sse_rx) = tokio::sync::mpsc::channel::<agentjail::ResourceStats>(16);
    let sse_sampler = handle.cgroup_path().map(|p| {
        sampler::spawn(p, std::time::Duration::from_millis(500), move |s| {
            let _ = sse_tx.try_send(s);
        })
    });

    // Keep tempdirs + permit alive for the whole stream lifetime.
    let keepalive = (permit, started_guard, source_dir, output_dir);

    let stream = async_stream::stream! {
        // 1. started
        let started_payload = serde_json::json!({ "pid": pid });
        if let Ok(ev) = Event::default().event("started").json_data(started_payload) {
            yield Ok(ev);
        }

        // 2. drain stdout + stderr + stats until both EOFs
        let mut stdout_done = false;
        let mut stderr_done = false;
        while !stdout_done || !stderr_done {
            tokio::select! {
                biased;
                line = handle.stdout.read_line(), if !stdout_done => {
                    match line {
                        Some(l) => yield Ok(Event::default().event("stdout").data(trim_nl(l))),
                        None    => { stdout_done = true; }
                    }
                }
                line = handle.stderr.read_line(), if !stderr_done => {
                    match line {
                        Some(l) => yield Ok(Event::default().event("stderr").data(trim_nl(l))),
                        None    => { stderr_done = true; }
                    }
                }
                Some(s) = sse_rx.recv() => {
                    let payload = serde_json::json!({
                        "memory_peak_bytes":    s.memory_peak_bytes,
                        "memory_current_bytes": s.memory_current_bytes,
                        "cpu_usage_usec":       s.cpu_usage_usec,
                        "io_read_bytes":        s.io_read_bytes,
                        "io_write_bytes":       s.io_write_bytes,
                        "pids_current":         s.pids_current,
                    });
                    if let Ok(ev) = Event::default().event("stats").json_data(payload) {
                        yield Ok(ev);
                    }
                }
            }
        }

        // 3. wait for exit + collect stats — stop both samplers.
        if let Some(h) = stats_sampler { h.abort(); }
        if let Some(h) = sse_sampler   { h.abort(); }
        let output = handle.wait().await;
        let ev = match &output {
            Ok(o) => {
                let payload = serde_json::json!({
                    "exit_code":            o.exit_code,
                    "duration_ms":          u64::try_from(o.duration.as_millis()).unwrap_or(u64::MAX),
                    "timed_out":            o.timed_out,
                    "oom_killed":           o.oom_killed,
                    "memory_peak_bytes":    o.stats.as_ref().map(|s| s.memory_peak_bytes).unwrap_or(0),
                    "memory_current_bytes": o.stats.as_ref().map(|s| s.memory_current_bytes).unwrap_or(0),
                    "cpu_usage_usec":       o.stats.as_ref().map(|s| s.cpu_usage_usec).unwrap_or(0),
                    "pids_current":         o.stats.as_ref().map(|s| s.pids_current).unwrap_or(0),
                });
                Event::default().event("completed").json_data(payload)
            }
            Err(e) => Event::default().event("error").json_data(
                serde_json::json!({ "message": e.to_string() })
            ),
        };
        match &output {
            Ok(o)  => jails_store.finish(stream_rec, o).await,
            Err(e) => jails_store.error(stream_rec, e.to_string()).await,
        }
        if let Ok(ev) = ev { yield Ok(ev); }

        drop(keepalive);
    };

    Ok(Sse::new(stream.boxed()).keep_alive(KeepAlive::default()))
}

fn trim_nl(mut s: String) -> String {
    if s.ends_with('\n') { s.pop(); }
    if s.ends_with('\r') { s.pop(); }
    s
}
