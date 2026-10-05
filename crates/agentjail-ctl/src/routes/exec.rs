//! Core execution — shared types, helpers, and the two non-streaming
//! handlers (`exec_in_session`, `create_run`). [`stream`] and [`fork`]
//! reuse the types and helpers defined here.
//!
//! [`stream`]: super::stream
//! [`fork`]: super::fork

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{CtlError, Result};
use crate::jails::JailKind;

use super::AppState;
use super::exec_git::git_clone;
use super::exec_monitor::run_monitored;

// ---------- shared request shapes ----------

#[derive(Debug, Deserialize, Default)]
pub(super) struct ExecOptions {
    /// Network policy. Omit (or `{"mode":"none"}`) for no network.
    #[serde(default)]
    pub(super) network: Option<NetworkSpec>,
    /// Seccomp level. "disabled" is intentionally not exposed.
    #[serde(default)]
    pub(super) seccomp: Option<SeccompSpec>,
    /// CPU quota (100 = one full core). Clamped to 1..=800.
    #[serde(default)]
    pub(super) cpu_percent: Option<u64>,
    /// PID cap. Clamped to 1..=1024.
    #[serde(default)]
    pub(super) max_pids: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub(super) enum NetworkSpec {
    None,
    Loopback,
    Allowlist {
        /// Domains (or globs like `*.api.example.com`). Non-empty, ≤ 32.
        #[serde(default)]
        domains: Vec<String>,
    },
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(super) enum SeccompSpec {
    Standard,
    Strict,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExecRequest {
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    timeout_secs: Option<u64>,
    memory_mb: Option<u64>,
    #[serde(flatten)]
    options: ExecOptions,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RunRequest {
    pub(super) code: String,
    #[serde(default = "default_language")]
    pub(super) language: String,
    pub(super) timeout_secs: Option<u64>,
    pub(super) memory_mb: Option<u64>,
    /// When present, the server `git clone`s this repo into the jail's
    /// source directory before running the code. The checkout is available
    /// inside the jail at `/workspace`.
    #[serde(default)]
    pub(super) git: Option<GitSpec>,
    #[serde(flatten)]
    pub(super) options: ExecOptions,
}

#[derive(Debug, Deserialize, Clone)]
pub(super) struct GitSpec {
    /// Single-repo form: `https://…` URL. Mutually exclusive with `repos`.
    #[serde(default)]
    pub(super) repo: Option<String>,
    /// Optional branch / tag / commit for the single-repo form.
    #[serde(default, rename = "ref")]
    pub(super) git_ref: Option<String>,
    /// Multi-repo form — each entry clones into its own subdirectory
    /// named after the repo basename. Use for cursor/devin-style agents
    /// that work across several checkouts at once.
    #[serde(default)]
    pub(super) repos: Vec<GitRepoEntry>,
}

/// One entry in a multi-repo clone.
#[derive(Debug, Deserialize, Clone)]
pub(super) struct GitRepoEntry {
    pub(super) repo: String,
    #[serde(default, rename = "ref")]
    pub(super) git_ref: Option<String>,
    /// Directory name under the workspace root. Defaults to the last
    /// path segment of `repo` (stripped of a trailing `.git`).
    #[serde(default)]
    pub(super) dir: Option<String>,
}

impl GitSpec {
    /// Return `(primary_repo_url, primary_ref)` for the audit
    /// `git_repo` / `git_ref` columns. Uses the single-repo fields when
    /// set, else the first `repos[]` entry.
    pub(super) fn primary(&self) -> (Option<String>, Option<String>) {
        if let Some(repo) = &self.repo {
            (Some(repo.clone()), self.git_ref.clone())
        } else if let Some(first) = self.repos.first() {
            (Some(first.repo.clone()), first.git_ref.clone())
        } else {
            (None, None)
        }
    }
}

pub(super) fn default_language() -> String {
    "javascript".into()
}

/// Resolve a language string to `(file, command)` for the jailed runtime.
///
/// JavaScript snippets are written as `main.mjs` so Node loads them as
/// ESM. The CommonJS loader rejects top-level `await` — and playground
/// snippets almost always look like `const r = await fetch(...)` at the
/// top of the file, so ESM is what users actually want.
pub(super) fn language_runtime(lang: &str) -> Result<(&'static str, &'static str)> {
    match lang {
        "javascript" | "js" => Ok(("main.mjs", "/usr/bin/node")),
        "python" | "py"     => Ok(("main.py", "/usr/bin/python3")),
        "bash" | "sh"       => Ok(("main.sh", "/bin/sh")),
        other => Err(CtlError::BadRequest(format!("unsupported language: {other}"))),
    }
}

// ---------- shared response shape ----------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExecResponse {
    stdout: String,
    stderr: String,
    exit_code: i32,
    duration_ms: u64,
    timed_out: bool,
    oom_killed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<StatsResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatsResponse {
    memory_peak_bytes:    u64,
    memory_current_bytes: u64,
    cpu_usage_usec:       u64,
    io_read_bytes:        u64,
    io_write_bytes:       u64,
    pids_current:         u64,
}

/// Build the jail-ledger config snapshot from an exec-options block.
/// Every route that calls `JailStore::start()` passes the result of
/// this to `attach_config()` so the Jails page can show "what did this
/// run with?". Memory + timeout are the already-clamped values the
/// handler uses; defaults follow `jail_config()`'s clamps.
pub(super) fn config_snapshot(
    options: &ExecOptions,
    memory_mb: u64,
    timeout_secs: u64,
    git: Option<&GitSpec>,
) -> crate::JailConfigSnapshot {
    let (network_mode, network_domains) = match options.network.as_ref() {
        None | Some(NetworkSpec::None) => ("none".to_string(), vec![]),
        Some(NetworkSpec::Loopback)    => ("loopback".to_string(), vec![]),
        Some(NetworkSpec::Allowlist { domains }) => {
            ("allowlist".to_string(), domains.clone())
        }
    };
    let seccomp = match options.seccomp {
        None | Some(SeccompSpec::Standard) => "standard".to_string(),
        Some(SeccompSpec::Strict)          => "strict".to_string(),
    };
    let (git_repo, git_ref) = git.map(|g| g.primary()).unwrap_or((None, None));
    crate::JailConfigSnapshot {
        network_mode,
        network_domains,
        seccomp,
        memory_mb,
        timeout_secs,
        cpu_percent: options.cpu_percent.unwrap_or(100).clamp(1, 800),
        max_pids:    options.max_pids.unwrap_or(64).clamp(1, 1024),
        git_repo,
        git_ref,
    }
}

pub(super) fn output_to_response(output: agentjail::Output) -> ExecResponse {
    ExecResponse {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.exit_code,
        duration_ms: output.duration.as_millis() as u64,
        timed_out: output.timed_out,
        oom_killed: output.oom_killed,
        stats: output.stats.map(|s| StatsResponse {
            memory_peak_bytes:    s.memory_peak_bytes,
            memory_current_bytes: s.memory_current_bytes,
            cpu_usage_usec:       s.cpu_usage_usec,
            io_read_bytes:        s.io_read_bytes,
            io_write_bytes:       s.io_write_bytes,
            pids_current:         s.pids_current,
        }),
    }
}

// ---------- handlers ----------

/// `POST /v1/sessions/:id/exec` — run a command in a session's jail.
pub(crate) async fn exec_in_session(
    State(state): State<AppState>,
    scope: crate::tenant::TenantScope,
    Path(id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResponse>> {
    let exec_cfg = state.exec_config.as_ref()
        .ok_or_else(|| CtlError::Internal("exec not enabled".into()))?;

    let session = state.sessions.get(&id).await
        .filter(|s| scope.can_see(&s.tenant_id))
        .ok_or_else(|| CtlError::NotFound(format!("session {id}")))?;

    // Enforce session expiry.
    if let Some(exp) = session.expires_at {
        if exp < OffsetDateTime::now_utc() {
            return Err(CtlError::BadRequest("session expired".into()));
        }
    }

    // Validate inputs.
    let timeout = req.timeout_secs.unwrap_or(exec_cfg.default_timeout_secs).clamp(1, 3600);
    let memory = req.memory_mb.unwrap_or(exec_cfg.default_memory_mb).min(8192);

    // Acquire exec permit (bounded concurrency).
    let _permit = state.exec_semaphore.try_acquire()
        .map_err(|_| CtlError::BadRequest("too many concurrent executions".into()))?;
    let _guard = state.exec_metrics.start();

    let env: Vec<(String, String)> = std::iter::once(("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()))
        .chain(session.env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .collect();

    let source = tempfile::tempdir().map_err(CtlError::Io)?;
    let output = tempfile::tempdir().map_err(CtlError::Io)?;

    let config = jail_config(
        source.path(), output.path(), memory, timeout, env, &req.options, /* source_rw */ false,
        /* no flavors for one-shot session exec — jail lives ~1 request */
        Vec::new(),
    )?;
    let jail = agentjail::Jail::new(config)?;
    let args_refs: Vec<&str> = req.args.iter().map(|s| s.as_str()).collect();

    let rec_id = state.jails
        .start(scope.tenant.clone(), JailKind::Exec, req.cmd.clone(), Some(id.clone()), None)
        .await;
    state.jails.attach_config(rec_id, config_snapshot(&req.options, memory, timeout, None)).await;
    let result = run_monitored(&state.jails, rec_id, &jail, &req.cmd, &args_refs).await;
    let result = match result {
        Ok(r)  => { state.jails.finish(rec_id, &r).await; r }
        Err(e) => { state.jails.error(rec_id, e.to_string()).await; return Err(e.into()); }
    };

    tracing::info!(
        session_id = %id,
        cmd = %req.cmd,
        exit_code = result.exit_code,
        duration_ms = result.duration.as_millis() as u64,
        timed_out = result.timed_out,
        oom_killed = result.oom_killed,
        memory_peak = result.stats.as_ref().map(|s| s.memory_peak_bytes).unwrap_or(0),
        "exec completed"
    );

    Ok(Json(output_to_response(result)))
}

/// `POST /v1/runs` — one-shot code execution.
pub(crate) async fn create_run(
    State(state): State<AppState>,
    scope: crate::tenant::TenantScope,
    Json(req): Json<RunRequest>,
) -> Result<(StatusCode, Json<ExecResponse>)> {
    let exec_cfg = state.exec_config.as_ref()
        .ok_or_else(|| CtlError::Internal("exec not enabled".into()))?;

    // Validate.
    if req.code.len() > 1024 * 1024 {
        return Err(CtlError::BadRequest("code exceeds 1 MB".into()));
    }
    let timeout = req.timeout_secs.unwrap_or(exec_cfg.default_timeout_secs).clamp(1, 3600);
    let memory = req.memory_mb.unwrap_or(exec_cfg.default_memory_mb).min(8192);

    let _permit = state.exec_semaphore.try_acquire()
        .map_err(|_| CtlError::BadRequest("too many concurrent executions".into()))?;
    let _guard = state.exec_metrics.start();

    let source = tempfile::tempdir().map_err(CtlError::Io)?;
    let output_dir = tempfile::tempdir().map_err(CtlError::Io)?;

    let (filename, cmd) = language_runtime(&req.language)?;

    // Optional: `git clone` into the source tree before mounting.
    if let Some(g) = &req.git { git_clone(g, source.path()).await?; }

    std::fs::write(source.path().join(filename), &req.code).map_err(CtlError::Io)?;

    let run_env = vec![("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into())];
    let config = jail_config(
        source.path(), output_dir.path(), memory, timeout, run_env, &req.options,
        /* source_rw */ false,
        Vec::new(),
    )?;

    let jail = agentjail::Jail::new(config)?;
    let rec_id = state.jails
        .start(scope.tenant.clone(), JailKind::Run, req.language.clone(), None, None)
        .await;
    state.jails.attach_config(
        rec_id,
        config_snapshot(&req.options, memory, timeout, req.git.as_ref()),
    ).await;
    let args: Vec<String> = vec![format!("/workspace/{filename}")];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = run_monitored(&state.jails, rec_id, &jail, cmd, &args_ref).await;
    let result = match result {
        Ok(r)  => { state.jails.finish(rec_id, &r).await; r }
        Err(e) => { state.jails.error(rec_id, e.to_string()).await; return Err(e.into()); }
    };

    tracing::info!(
        language = %req.language,
        exit_code = result.exit_code,
        duration_ms = result.duration.as_millis() as u64,
        timed_out = result.timed_out,
        "run completed"
    );

    Ok((StatusCode::CREATED, Json(output_to_response(result))))
}

/// Build a JailConfig with sensible defaults for API-driven execution.
/// Options are clamped; illegal allowlists return `400`.
///
/// `source_rw` switches the `/workspace` mount between read-only (the
/// default — one-shot runs where the seed is immutable) and read-write
/// (persistent workspaces, where the jail mutates its own tree).
pub(super) fn jail_config(
    source: &std::path::Path,
    output: &std::path::Path,
    memory_mb: u64,
    timeout_secs: u64,
    env: Vec<(String, String)>,
    options: &ExecOptions,
    source_rw: bool,
    readonly_overlays: Vec<std::path::PathBuf>,
) -> Result<agentjail::JailConfig> {
    let network = match options.network.as_ref() {
        None | Some(NetworkSpec::None) => agentjail::Network::None,
        Some(NetworkSpec::Loopback)    => agentjail::Network::Loopback,
        Some(NetworkSpec::Allowlist { domains }) => {
            validate_domains(domains)?;
            agentjail::Network::Allowlist(domains.clone())
        }
    };
    let seccomp = match options.seccomp {
        None | Some(SeccompSpec::Standard) => agentjail::SeccompLevel::Standard,
        Some(SeccompSpec::Strict)          => agentjail::SeccompLevel::Strict,
    };
    let cpu_percent = options.cpu_percent.unwrap_or(100).clamp(1, 800);
    let max_pids    = options.max_pids.unwrap_or(64).clamp(1, 1024);

    let is_root = unsafe { libc::getuid() == 0 };
    Ok(agentjail::JailConfig {
        source: source.into(),
        output: output.into(),
        source_rw,
        network,
        seccomp,
        landlock: false,
        memory_mb,
        cpu_percent,
        max_pids,
        timeout_secs,
        user_namespace: !is_root,
        pid_namespace: true,
        env,
        readonly_overlays,
        ..Default::default()
    })
}

fn validate_domains(domains: &[String]) -> Result<()> {
    if domains.is_empty() {
        return Err(CtlError::BadRequest(
            "allowlist must contain at least one domain".into(),
        ));
    }
    if domains.len() > 32 {
        return Err(CtlError::BadRequest(
            "allowlist limited to 32 domains".into(),
        ));
    }
    for d in domains {
        if d.is_empty() || d.len() > 253 {
            return Err(CtlError::BadRequest(format!("invalid domain: {d:?}")));
        }
        if d.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(CtlError::BadRequest(format!("invalid domain: {d:?}")));
        }
        if d.contains("://") {
            return Err(CtlError::BadRequest(format!(
                "domain must not include scheme: {d:?}"
            )));
        }
    }
    Ok(())
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn opts() -> ExecOptions { ExecOptions::default() }

    #[test]
    fn defaults_are_none_and_standard() {
        let c = jail_config(
            &PathBuf::from("/tmp/src"),
            &PathBuf::from("/tmp/out"),
            256, 60, vec![], &opts(), false, Vec::new(),
        ).unwrap();
        assert!(matches!(c.network, agentjail::Network::None));
        assert!(matches!(c.seccomp, agentjail::SeccompLevel::Standard));
        assert_eq!(c.cpu_percent, 100);
        assert_eq!(c.max_pids, 64);
    }

    #[test]
    fn allowlist_roundtrips() {
        let o = ExecOptions {
            network: Some(NetworkSpec::Allowlist {
                domains: vec!["api.openai.com".into(), "*.mcp.example.com".into()],
            }),
            seccomp: Some(SeccompSpec::Strict),
            cpu_percent: Some(200),
            max_pids: Some(128),
        };
        let c = jail_config(
            &PathBuf::from("/tmp/src"),
            &PathBuf::from("/tmp/out"),
            256, 60, vec![], &o, false, Vec::new(),
        ).unwrap();
        match c.network {
            agentjail::Network::Allowlist(d) => assert_eq!(d.len(), 2),
            _ => panic!("expected allowlist"),
        }
        assert!(matches!(c.seccomp, agentjail::SeccompLevel::Strict));
        assert_eq!(c.cpu_percent, 200);
        assert_eq!(c.max_pids, 128);
    }

    #[test]
    fn cpu_and_pids_are_clamped() {
        let o = ExecOptions {
            cpu_percent: Some(9_999),
            max_pids: Some(100_000),
            ..Default::default()
        };
        let c = jail_config(
            &PathBuf::from("/tmp/src"),
            &PathBuf::from("/tmp/out"),
            256, 60, vec![], &o, false, Vec::new(),
        ).unwrap();
        assert_eq!(c.cpu_percent, 800);
        assert_eq!(c.max_pids, 1024);
    }

    #[test]
    fn empty_allowlist_is_rejected() {
        let o = ExecOptions {
            network: Some(NetworkSpec::Allowlist { domains: vec![] }),
            ..Default::default()
        };
        assert!(jail_config(
            &PathBuf::from("/tmp/src"), &PathBuf::from("/tmp/out"),
            256, 60, vec![], &o, false, Vec::new(),
        ).is_err());
    }

    #[test]
    fn domain_with_scheme_is_rejected() {
        let o = ExecOptions {
            network: Some(NetworkSpec::Allowlist {
                domains: vec!["https://api.openai.com".into()],
            }),
            ..Default::default()
        };
        assert!(jail_config(
            &PathBuf::from("/tmp/src"), &PathBuf::from("/tmp/out"),
            256, 60, vec![], &o, false, Vec::new(),
        ).is_err());
    }

    #[test]
    fn allowlist_accepts_string_or_object_network_forms() {
        // object form: {"mode":"allowlist","domains":[...]}
        let json = serde_json::json!({
            "mode": "allowlist",
            "domains": ["api.openai.com"]
        });
        let n: NetworkSpec = serde_json::from_value(json).unwrap();
        match n {
            NetworkSpec::Allowlist { domains } => assert_eq!(domains, vec!["api.openai.com"]),
            _ => panic!("expected allowlist"),
        }

        // plain modes
        let n: NetworkSpec = serde_json::from_str(r#"{"mode":"none"}"#).unwrap();
        assert!(matches!(n, NetworkSpec::None));
        let n: NetworkSpec = serde_json::from_str(r#"{"mode":"loopback"}"#).unwrap();
        assert!(matches!(n, NetworkSpec::Loopback));
    }

    #[test]
    fn run_request_accepts_flattened_options() {
        let json = serde_json::json!({
            "code": "console.log(1)",
            "language": "javascript",
            "memory_mb": 128,
            "seccomp": "strict",
            "cpu_percent": 150,
            "network": { "mode": "loopback" }
        });
        let r: RunRequest = serde_json::from_value(json).unwrap();
        assert_eq!(r.memory_mb, Some(128));
        assert!(matches!(r.options.seccomp, Some(SeccompSpec::Strict)));
        assert_eq!(r.options.cpu_percent, Some(150));
    }
}
