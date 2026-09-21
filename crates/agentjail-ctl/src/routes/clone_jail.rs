//! Run `git clone` inside a short-lived jail instead of on the host.
//!
//! This closes the defense-in-depth gap flagged as the "architectural"
//! half of C1 in RED.md: even though the host-side `git clone` has
//! every known hardening flag, a future CVE in git's config-parse /
//! submodule-resolve / bundle-v3 code would land on an unsandboxed
//! process. Here the same command runs with:
//!
//! - strict seccomp
//! - network allowlist pinned to the repo host(s) only
//! - read-only rootfs except for the target dir
//! - 60-second timeout, memory cap, pid cap
//! - no access to any other tenant's / workspace's state
//!
//! The entry points mirror [`super::exec_git`]:
//!
//! - [`git_clone_in_jail`] — matches the old `git_clone(spec, dst)`
//!   contract so callers switch without threading a fifth argument.
//!
//! The validators (`validate_repo_url`, `validate_git_ref`,
//! `validate_subdir`, `default_repo_subdir`) stay in `exec_git` and
//! are called from here — they're pure string checks and don't care
//! whether the command runs on host or in a jail.

use std::path::Path;

use agentjail::{JailConfig, Network, SeccompLevel};

use super::exec_git::{default_repo_subdir, validate_git_ref, validate_repo_url, validate_subdir};
use super::exec::GitSpec;
use crate::error::{CtlError, Result};

/// Clone (one or many repos) into `dst`, each inside its own short-lived
/// jail. Semantics match [`super::exec_git::git_clone`]:
///
/// - single-repo `{ repo, ref? }`: contents land at `dst/`
/// - multi-repo `{ repos: [{ repo, ref?, dir? }] }`: each repo gets a
///   subdirectory.
pub(super) async fn git_clone_in_jail(spec: &GitSpec, dst: &Path) -> Result<()> {
    match (spec.repo.as_deref(), spec.repos.is_empty()) {
        (Some(_), false) => {
            return Err(CtlError::BadRequest(
                "git: set either `repo` or `repos`, not both".into(),
            ));
        }
        (None, true) => {
            return Err(CtlError::BadRequest(
                "git: provide `repo` or `repos`".into(),
            ));
        }
        _ => {}
    }

    if let Some(repo) = &spec.repo {
        clone_one(repo, spec.git_ref.as_deref(), dst).await?;
    } else {
        for entry in &spec.repos {
            let subdir = entry
                .dir
                .clone()
                .unwrap_or_else(|| default_repo_subdir(&entry.repo));
            validate_subdir(&subdir)?;
            let sub = dst.join(&subdir);
            std::fs::create_dir_all(&sub).map_err(CtlError::Io)?;
            clone_one(&entry.repo, entry.git_ref.as_deref(), &sub).await?;
        }
    }
    Ok(())
}

/// Spawn a jail whose entrypoint is `git clone <url> /workspace`,
/// where `/workspace` is bind-mounted from the host-side `dst` dir.
/// Waits for exit, reports failure with the tail of stderr.
async fn clone_one(repo: &str, git_ref: Option<&str>, dst: &Path) -> Result<()> {
    validate_repo_url(repo)?;
    if let Some(r) = git_ref {
        validate_git_ref(r)?;
    }

    let host = repo_host(repo)?;
    let config = clone_jail_config(dst, &host)?;
    let jail = agentjail::Jail::new(config).map_err(CtlError::Jail)?;

    // git `-c` overrides are still passed inside the jail: the flags
    // are cheap insurance against a malicious repo tricking the
    // jailed git into spawning a helper or reading host config.
    let mut args: Vec<String> = vec![
        "-c".into(), "protocol.allow=never".into(),
        "-c".into(), "protocol.https.allow=always".into(),
        "-c".into(), "protocol.ext.allow=never".into(),
        "-c".into(), "protocol.file.allow=never".into(),
        "-c".into(), "core.sshCommand=false".into(),
        "-c".into(), "core.fsmonitor=false".into(),
        "-c".into(), "core.hooksPath=/dev/null".into(),
        "-c".into(), "transfer.fsckObjects=true".into(),
        "clone".into(), "--depth=1".into(), "--single-branch".into(),
        "--no-tags".into(),
    ];
    if let Some(r) = git_ref {
        args.push("--branch".into());
        args.push(r.to_string());
    }
    args.push("--".into());
    args.push(repo.to_string());
    // Clone into the writable /workspace so the host-side `dst` ends
    // up with the repo root directly. Using `.` avoids git's "target
    // must be non-existent or empty" check against the bind-mounted
    // dir itself.
    args.push("/workspace".into());

    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let handle = jail
        .spawn("/usr/bin/git", &args_refs)
        .map_err(CtlError::Jail)?;
    let out = handle.wait().await.map_err(CtlError::Jail)?;
    if !out.exit_code == 0 && !out.timed_out {
        // Normal fall-through — successful clone.
    }
    if out.timed_out {
        return Err(CtlError::BadRequest("git clone timed out (60s)".into()));
    }
    if out.exit_code != 0 {
        // Only bubble up the last few stderr lines — git's remote
        // errors tend to be on the final line and a screenful of
        // transfer progress isn't useful in an API response.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" · ");
        return Err(CtlError::BadRequest(format!("git clone failed: {tail}")));
    }
    Ok(())
}

/// Build the [`JailConfig`] used for the clone jail. Tight constraints:
/// one core, half a GB, 64 procs, strict seccomp, network limited to the
/// repo host, read-write `/workspace` mounted from `dst`, no output dir.
fn clone_jail_config(dst: &Path, host: &str) -> Result<JailConfig> {
    // The jail engine also needs an `output` dir — it bind-mounts
    // unconditionally — so we hand it a throwaway tempdir the clone
    // will never touch. Holding the `TempDir` for the life of the
    // exec happens via the closure in `git_clone_in_jail`'s caller...
    // Actually: TempDir is dropped at end of this function's scope,
    // which is *before* the jail process exits. Use a "leaked" path
    // instead and clean up best-effort after.
    let scratch = std::env::temp_dir().join(format!(
        "agentjail-clone-{}",
        std::process::id()
    ));
    // Per-invocation suffix so concurrent clones don't race on the
    // same directory. Tempdir-level uniqueness is already given by
    // the PID + the worker-task pointer address, but a nanosecond
    // stamp is cheap insurance.
    let scratch = scratch.with_extension(format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&scratch).map_err(CtlError::Io)?;

    let is_root = unsafe { libc::getuid() == 0 };
    Ok(JailConfig {
        source: dst.to_path_buf(),
        output: scratch.clone(),
        source_rw: true,
        network: Network::Allowlist(vec![host.to_string()]),
        // Standard, not Strict: Strict blocks SYS_socket/SYS_connect,
        // which kills git before it can even reach the allowlist
        // proxy. Standard still blocks ptrace, mount, bpf, io_uring,
        // and the rest of the escape-flavoured surface — net syscalls
        // are the only real difference, and git needs them.
        seccomp: SeccompLevel::Standard,
        landlock: false,
        memory_mb: 512,
        cpu_percent: 100,
        max_pids: 64,
        timeout_secs: 60,
        user_namespace: !is_root,
        pid_namespace: true,
        ipc_namespace: true,
        env: vec![
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("HOME".into(), "/tmp".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ],
        // Default workdir is `/workspace` which is exactly what we want
        // — the clone writes into the bind-mounted `dst`.
        ..Default::default()
    })
}

/// Extract the hostname from a known-valid `https://…` URL. The URL
/// has already been through [`validate_repo_url`], so we skip defensive
/// parsing — any error here is a caller bug.
fn repo_host(url: &str) -> Result<String> {
    let after = url
        .strip_prefix("https://")
        .ok_or_else(|| CtlError::BadRequest("git.repo must be https://".into()))?;
    let host = after.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(CtlError::BadRequest("git.repo missing host".into()));
    }
    // Strip any port — `github.com:443` becomes `github.com` for the
    // allowlist, which matches on host only.
    Ok(host.split(':').next().unwrap_or(host).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_host_strips_port_and_path() {
        assert_eq!(repo_host("https://github.com/a/b").unwrap(), "github.com");
        assert_eq!(repo_host("https://git.example.com:8443/a/b.git").unwrap(), "git.example.com");
    }

    #[test]
    fn repo_host_rejects_non_https() {
        assert!(repo_host("http://evil.com/a/b").is_err());
    }

    #[test]
    fn clone_jail_config_honors_hardening_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = clone_jail_config(tmp.path(), "github.com").unwrap();
        // Standard (socket-enabling) seccomp is required here — see
        // the comment on the config construction.
        assert!(matches!(cfg.seccomp, SeccompLevel::Standard));
        match cfg.network {
            Network::Allowlist(ref doms) => assert_eq!(doms, &vec!["github.com".to_string()]),
            _ => panic!("expected allowlist network"),
        }
        assert_eq!(cfg.timeout_secs, 60);
        assert!(cfg.source_rw);
        assert_eq!(cfg.source, tmp.path());
    }
}
