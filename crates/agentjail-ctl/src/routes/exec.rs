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
use super::exec_monitor::{JailRow, run_monitored};

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
    /// named after the repo basename. Use for background coding agents
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
        "python" | "py" => Ok(("main.py", "/usr/bin/python3")),
        "bash" | "sh" => Ok(("main.sh", "/bin/sh")),
        other => Err(CtlError::BadRequest(format!(
            "unsupported language: {other}"
        ))),
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
    memory_peak_bytes: u64,
    memory_current_bytes: u64,
    cpu_usage_usec: u64,
    io_read_bytes: u64,
    io_write_bytes: u64,
    pids_current: u64,
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
        Some(NetworkSpec::Loopback) => ("loopback".to_string(), vec![]),
        Some(NetworkSpec::Allowlist { domains }) => ("allowlist".to_string(), domains.clone()),
    };
    let seccomp = match options.seccomp {
        None | Some(SeccompSpec::Standard) => "standard".to_string(),
        Some(SeccompSpec::Strict) => "strict".to_string(),
    };
    let (git_repo, git_ref) = git.map(|g| g.primary()).unwrap_or((None, None));
    crate::JailConfigSnapshot {
        network_mode,
        network_domains,
        seccomp,
        memory_mb,
        timeout_secs,
        cpu_percent: options.cpu_percent.unwrap_or(100).clamp(1, 800),
        max_pids: options.max_pids.unwrap_or(64).clamp(1, 1024),
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
            memory_peak_bytes: s.memory_peak_bytes,
            memory_current_bytes: s.memory_current_bytes,
            cpu_usage_usec: s.cpu_usage_usec,
            io_read_bytes: s.io_read_bytes,
            io_write_bytes: s.io_write_bytes,
            pids_current: s.pids_current,
        }),
    }
}

/// Concurrency reservation for one request: `n` global semaphore
/// permits plus `n` per-tenant slots. Released on drop.
pub(super) struct ExecPermit {
    _global: tokio::sync::OwnedSemaphorePermit,
    _tenant: crate::exec::TenantPermit,
}

/// Reserve `n` jail slots (one per jail the request will spawn) for
/// `tenant`. Fails fast with 400 instead of queueing.
pub(super) fn acquire_exec(state: &AppState, tenant: &str, n: usize) -> Result<ExecPermit> {
    let n32 = u32::try_from(n.max(1))
        .map_err(|_| CtlError::BadRequest("too many jails requested".into()))?;
    let global = state
        .exec_semaphore
        .clone()
        .try_acquire_many_owned(n32)
        .map_err(|_| CtlError::BadRequest("too many concurrent executions".into()))?;
    let tenant = state
        .tenant_limiter
        .try_acquire(tenant, n.max(1))
        .ok_or_else(|| {
            CtlError::BadRequest("too many concurrent executions for this tenant".into())
        })?;
    Ok(ExecPermit {
        _global: global,
        _tenant: tenant,
    })
}

/// Write the user's code file into `dir` without following symlinks.
///
/// `dir` may contain a freshly cloned repo, and a repo can ship
/// `main.py -> /etc/cron.d/x`. A plain `fs::write` would follow that
/// link and write attacker-chosen bytes on the host. `O_NOFOLLOW`
/// refuses a symlink in the final component; `name` is a bare file
/// name, so there is no intermediate component to follow.
pub(super) fn write_code_file(dir: &std::path::Path, name: &str, code: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    debug_assert!(!name.contains('/'));
    let path = dir.join(name);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o644)
        .open(&path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                CtlError::BadRequest(format!("refusing to overwrite symlink {name:?}"))
            } else {
                CtlError::Io(e)
            }
        })?;
    // A repo can't ship a FIFO/device here through git, but be strict.
    let meta = f.metadata().map_err(CtlError::Io)?;
    if !meta.is_file() {
        return Err(CtlError::BadRequest(format!(
            "{name:?} is not a regular file"
        )));
    }
    f.write_all(code.as_bytes()).map_err(CtlError::Io)
}

// ---------- handlers ----------

/// `POST /v1/sessions/:id/exec` — run a command in a session's jail.
pub(crate) async fn exec_in_session(
    State(state): State<AppState>,
    scope: crate::tenant::TenantScope,
    Path(id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResponse>> {
    let exec_cfg = state
        .exec_config
        .as_ref()
        .ok_or_else(|| CtlError::Internal("exec not enabled".into()))?;

    let session = state
        .sessions
        .get(&id)
        .await
        .filter(|s| scope.can_see(&s.tenant_id))
        .ok_or_else(|| CtlError::NotFound(format!("session {id}")))?;

    // Enforce session expiry.
    if let Some(exp) = session.expires_at {
        if exp < OffsetDateTime::now_utc() {
            return Err(CtlError::BadRequest("session expired".into()));
        }
    }

    // Validate inputs.
    let timeout = req
        .timeout_secs
        .unwrap_or(exec_cfg.default_timeout_secs)
        .clamp(1, 3600);
    let memory = exec_cfg.clamp_memory(req.memory_mb, exec_cfg.default_memory_mb);

    // Acquire exec permit (bounded concurrency).
    let _permit = acquire_exec(&state, &scope.tenant, 1)?;
    let _guard = state.exec_metrics.start();

    let env: Vec<(String, String)> =
        std::iter::once(("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()))
            .chain(session.env.iter().map(|(k, v)| (k.clone(), v.clone())))
            .collect();

    let source = tempfile::tempdir().map_err(CtlError::Io)?;
    let output = tempfile::tempdir().map_err(CtlError::Io)?;

    let config = jail_config(
        exec_cfg,
        source.path(),
        output.path(),
        memory,
        timeout,
        env,
        &req.options,
        /* source_rw */ false,
        /* no flavors for one-shot session exec — jail lives ~1 request */
        Vec::new(),
    )?;
    let jail =
        super::exec_monitor::blocking(move || agentjail::Jail::new(config).map_err(CtlError::from))
            .await?;

    let rec_id = state
        .jails
        .start(
            scope.tenant.clone(),
            JailKind::Exec,
            req.cmd.clone(),
            Some(id.clone()),
            None,
        )
        .await;
    let row = JailRow::new(state.jails.clone(), rec_id);
    state
        .jails
        .attach_config(rec_id, config_snapshot(&req.options, memory, timeout, None))
        .await;
    let result = run_monitored(
        &state.jails,
        row,
        jail,
        &req.cmd,
        &req.args,
        std::time::Duration::from_secs(timeout),
    )
    .await?;

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
    let exec_cfg = state
        .exec_config
        .as_ref()
        .ok_or_else(|| CtlError::Internal("exec not enabled".into()))?;

    // Validate.
    if req.code.len() > 1024 * 1024 {
        return Err(CtlError::BadRequest("code exceeds 1 MB".into()));
    }
    let timeout = req
        .timeout_secs
        .unwrap_or(exec_cfg.default_timeout_secs)
        .clamp(1, 3600);
    let memory = exec_cfg.clamp_memory(req.memory_mb, exec_cfg.default_memory_mb);

    let _permit = acquire_exec(&state, &scope.tenant, 1)?;
    let _guard = state.exec_metrics.start();

    let source = tempfile::tempdir().map_err(CtlError::Io)?;
    let output_dir = tempfile::tempdir().map_err(CtlError::Io)?;

    let (filename, cmd) = language_runtime(&req.language)?;

    // Optional: `git clone` into the source tree before mounting.
    if let Some(g) = &req.git {
        git_clone(g, source.path()).await?;
    }

    write_code_file(source.path(), filename, &req.code)?;

    let run_env = vec![("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into())];
    let config = jail_config(
        exec_cfg,
        source.path(),
        output_dir.path(),
        memory,
        timeout,
        run_env,
        &req.options,
        /* source_rw */ false,
        Vec::new(),
    )?;

    let jail =
        super::exec_monitor::blocking(move || agentjail::Jail::new(config).map_err(CtlError::from))
            .await?;
    let rec_id = state
        .jails
        .start(
            scope.tenant.clone(),
            JailKind::Run,
            req.language.clone(),
            None,
            None,
        )
        .await;
    let row = JailRow::new(state.jails.clone(), rec_id);
    state
        .jails
        .attach_config(
            rec_id,
            config_snapshot(&req.options, memory, timeout, req.git.as_ref()),
        )
        .await;
    let args: Vec<String> = vec![format!("/workspace/{filename}")];
    let result = run_monitored(
        &state.jails,
        row,
        jail,
        cmd,
        &args,
        std::time::Duration::from_secs(timeout),
    )
    .await?;

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
#[allow(clippy::too_many_arguments)]
pub(super) fn jail_config(
    exec_cfg: &crate::exec::ExecConfig,
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
        Some(NetworkSpec::Loopback) => agentjail::Network::Loopback,
        Some(NetworkSpec::Allowlist { domains }) => {
            validate_domains(domains)?;
            agentjail::Network::Allowlist(domains.clone())
        }
    };
    let seccomp = match options.seccomp {
        None | Some(SeccompSpec::Standard) => agentjail::SeccompLevel::Standard,
        Some(SeccompSpec::Strict) => agentjail::SeccompLevel::Strict,
    };
    let cpu_percent = options.cpu_percent.unwrap_or(100).clamp(1, 800);
    let max_pids = options.max_pids.unwrap_or(64).clamp(1, 1024);

    Ok(agentjail::JailConfig {
        source: source.into(),
        output: output.into(),
        source_rw,
        network,
        seccomp,
        // Hardening comes from operator config (safe defaults: both on),
        // not from "are we root?".
        landlock: exec_cfg.landlock,
        // Always a real limit: the clamp keeps it >= 1, and the API never
        // asks the engine for "no memory limit" (`None`).
        memory_mb: Some(memory_mb.max(exec_cfg.min_memory_mb.max(1))),
        cpu_percent,
        max_pids,
        timeout_secs,
        user_namespace: exec_cfg.user_namespace,
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

    fn opts() -> ExecOptions {
        ExecOptions::default()
    }

    #[test]
    fn defaults_are_none_and_standard() {
        let c = jail_config(
            &crate::exec::ExecConfig::default(),
            &PathBuf::from("/tmp/src"),
            &PathBuf::from("/tmp/out"),
            256,
            60,
            vec![],
            &opts(),
            false,
            Vec::new(),
        )
        .unwrap();
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
            &crate::exec::ExecConfig::default(),
            &PathBuf::from("/tmp/src"),
            &PathBuf::from("/tmp/out"),
            256,
            60,
            vec![],
            &o,
            false,
            Vec::new(),
        )
        .unwrap();
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
            &crate::exec::ExecConfig::default(),
            &PathBuf::from("/tmp/src"),
            &PathBuf::from("/tmp/out"),
            256,
            60,
            vec![],
            &o,
            false,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(c.cpu_percent, 800);
        assert_eq!(c.max_pids, 1024);
    }

    #[test]
    fn empty_allowlist_is_rejected() {
        let o = ExecOptions {
            network: Some(NetworkSpec::Allowlist { domains: vec![] }),
            ..Default::default()
        };
        assert!(
            jail_config(
                &crate::exec::ExecConfig::default(),
                &PathBuf::from("/tmp/src"),
                &PathBuf::from("/tmp/out"),
                256,
                60,
                vec![],
                &o,
                false,
                Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn domain_with_scheme_is_rejected() {
        let o = ExecOptions {
            network: Some(NetworkSpec::Allowlist {
                domains: vec!["https://api.openai.com".into()],
            }),
            ..Default::default()
        };
        assert!(
            jail_config(
                &crate::exec::ExecConfig::default(),
                &PathBuf::from("/tmp/src"),
                &PathBuf::from("/tmp/out"),
                256,
                60,
                vec![],
                &o,
                false,
                Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn hardening_follows_config_not_uid() {
        let mut cfg = crate::exec::ExecConfig::default();
        let c = jail_config(
            &cfg,
            &PathBuf::from("/tmp/src"),
            &PathBuf::from("/tmp/out"),
            0,
            60,
            vec![],
            &opts(),
            false,
            Vec::new(),
        )
        .unwrap();
        // Safe defaults regardless of whether the server runs as root.
        assert!(c.user_namespace);
        assert!(c.landlock);
        // A request of 0 is clamped up; it never becomes "no limit".
        assert_eq!(c.memory_mb, Some(64));

        cfg.user_namespace = false;
        cfg.landlock = false;
        let c = jail_config(
            &cfg,
            &PathBuf::from("/tmp/src"),
            &PathBuf::from("/tmp/out"),
            256,
            60,
            vec![],
            &opts(),
            false,
            Vec::new(),
        )
        .unwrap();
        assert!(!c.user_namespace);
        assert!(!c.landlock);
    }

    #[test]
    fn write_code_file_refuses_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let victim = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(victim.path(), b"original").unwrap();
        std::os::unix::fs::symlink(victim.path(), dir.path().join("main.py")).unwrap();

        let err = write_code_file(dir.path(), "main.py", "pwned").unwrap_err();
        assert!(matches!(err, CtlError::BadRequest(_)), "got {err:?}");
        assert_eq!(std::fs::read(victim.path()).unwrap(), b"original");
    }

    #[test]
    fn write_code_file_overwrites_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.py"), b"old").unwrap();
        write_code_file(dir.path(), "main.py", "new").unwrap();
        assert_eq!(std::fs::read(dir.path().join("main.py")).unwrap(), b"new");
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
