//! Host-side `git clone` for seeding a jail's source directory.
//!
//! Extracted from [`super::exec`] because the workflow is entirely
//! host-side (runs before the jail locks down the filesystem) and has
//! its own validation surface: URL scheme, length caps, subdir safety,
//! per-clone 60 s timeout.

use super::exec::GitSpec;
use crate::error::{CtlError, Result};

/// Shallow-clone one or more repos into the jail's source directory.
/// Runs on the host, *before* the jail locks down the filesystem.
///
/// - Single-repo form (`{ repo, ref? }`): contents land at the root of
///   `dst` (back-compat with earlier versions).
/// - Multi-repo form (`{ repos: [{ repo, ref?, dir? }] }`): each repo
///   clones into its own subdirectory under `dst`. Default subdir
///   name is the repo basename (with any `.git` suffix stripped).
///
/// Security: every URL must be `https://` (no ssh/git/file), URL ≤ 512
/// bytes, ref ≤ 200, git runs with `--depth=1` and a 60 s hard timeout.
pub(super) async fn git_clone(spec: &GitSpec, dst: &std::path::Path) -> Result<()> {
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

/// Shallow-clone a single repo into `dst`, flattening the clone dir so
/// `dst` ends up at the repo root.
///
/// Runs on the host before the jail is built, so every attacker-facing
/// surface gets locked down here: reject non-`https` URLs and URLs with
/// `userinfo` (tokens leaking into the ledger), reject refs starting with
/// `-` (flag injection, CVE-2018-17456 class), force `--` to separate the
/// ref from positional args, and pin git itself via `-c` overrides that
/// defeat the usual "malicious repo runs code on clone" tricks
/// (`core.sshCommand`, `core.fsmonitor`, `core.hooksPath`, external
/// submodule protocols, pre-receive hook objects). Env is scrubbed to
/// prevent `GIT_SSH_COMMAND` / `GIT_EXEC_PATH` / `GIT_CONFIG_*` inheritance.
async fn clone_one(repo: &str, git_ref: Option<&str>, dst: &std::path::Path) -> Result<()> {
    validate_repo_url(repo)?;
    if let Some(r) = git_ref {
        validate_git_ref(r)?;
    }

    // git clone refuses to clone into a non-empty dir, so stage into a
    // subpath and flatten afterwards.
    let target = dst.join("__repo");
    std::fs::create_dir_all(&target).map_err(CtlError::Io)?;

    let mut cmd = tokio::process::Command::new("git");
    // Config pins applied *before* the `clone` subcommand so they bind
    // for the whole invocation, including submodule recursion.
    cmd.arg("-c").arg("protocol.allow=never")
        .arg("-c").arg("protocol.https.allow=always")
        .arg("-c").arg("protocol.ext.allow=never")
        .arg("-c").arg("protocol.file.allow=never")
        .arg("-c").arg("core.sshCommand=false")
        .arg("-c").arg("core.fsmonitor=false")
        .arg("-c").arg("core.hooksPath=/dev/null")
        .arg("-c").arg("transfer.fsckObjects=true")
        .arg("-c").arg("fetch.fsckObjects=true")
        .arg("-c").arg("receive.fsckObjects=true")
        .arg("clone").arg("--depth=1").arg("--single-branch")
        .arg("--no-tags");
    if let Some(r) = git_ref {
        cmd.arg("--branch").arg(r);
    }
    // `--` guarantees the repo url is treated as a positional, never a
    // flag — belt-and-braces next to the `!repo.starts_with("https://")`
    // check above.
    cmd.arg("--").arg(repo).arg(&target);
    cmd.kill_on_drop(true);
    // Inherited env is a known RCE vector: `GIT_SSH_COMMAND`,
    // `GIT_EXEC_PATH`, `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_*`, etc.
    cmd.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/tmp")
        .env("GIT_TERMINAL_PROMPT", "0");

    let out = tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| CtlError::BadRequest("git clone timed out (60s)".into()))?
        .map_err(CtlError::Io)?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" · ");
        return Err(CtlError::BadRequest(format!("git clone failed: {tail}")));
    }

    for entry in std::fs::read_dir(&target).map_err(CtlError::Io)? {
        let e = entry.map_err(CtlError::Io)?;
        let from = e.path();
        let to = dst.join(e.file_name());
        std::fs::rename(&from, &to).map_err(CtlError::Io)?;
    }
    let _ = std::fs::remove_dir_all(&target);
    Ok(())
}

pub(super) fn default_repo_subdir(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .trim()
        .to_string()
}

pub(super) fn validate_subdir(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(CtlError::BadRequest(format!("git.repos.dir invalid: {name:?}")));
    }
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Err(CtlError::BadRequest(format!(
            "git.repos.dir must not contain slashes or leading `.`: {name:?}"
        )));
    }
    Ok(())
}

/// Security-critical: all the ways a malicious URL could subvert the
/// host-side clone collapse into this one function.
pub(super) fn validate_repo_url(repo: &str) -> Result<()> {
    if !repo.starts_with("https://") || repo.len() > 512 {
        return Err(CtlError::BadRequest(
            "git.repo must be https:// (max 512 bytes)".into(),
        ));
    }
    // `https://user:token@host/repo` — `git clone` accepts this and the
    // token ends up in the ledger forever. Reject at ingest.
    let host = repo["https://".len()..].split('/').next().unwrap_or("");
    if host.contains('@') {
        return Err(CtlError::BadRequest(
            "git.repo must not embed credentials (user:token@host)".into(),
        ));
    }
    Ok(())
}

/// Refs that start with `-` get interpreted as git flags
/// (`--upload-pack=…` → RCE, CVE-2018-17456 class). Also block control
/// chars and oversize inputs.
pub(super) fn validate_git_ref(r: &str) -> Result<()> {
    if r.is_empty() || r.len() > 200 || r.chars().any(|c| c.is_control()) {
        return Err(CtlError::BadRequest("git.ref invalid".into()));
    }
    if r.starts_with('-') {
        return Err(CtlError::BadRequest(
            "git.ref must not start with `-`".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_url_requires_https() {
        assert!(validate_repo_url("https://github.com/a/b").is_ok());
        assert!(validate_repo_url("http://github.com/a/b").is_err());
        assert!(validate_repo_url("ssh://git@github.com/a/b").is_err());
        assert!(validate_repo_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn repo_url_rejects_userinfo() {
        assert!(validate_repo_url("https://x:ghp_AAA@github.com/a/b").is_err());
        assert!(validate_repo_url("https://user@github.com/a/b").is_err());
        // An `@` later in the path is fine — only the host segment is checked.
        assert!(validate_repo_url("https://github.com/a/b@tag").is_ok());
    }

    #[test]
    fn repo_url_length_capped() {
        let long = format!("https://github.com/{}", "a".repeat(600));
        assert!(validate_repo_url(&long).is_err());
    }

    #[test]
    fn git_ref_rejects_flag_injection() {
        assert!(validate_git_ref("main").is_ok());
        assert!(validate_git_ref("v1.2.3").is_ok());
        assert!(validate_git_ref("-upload-pack=/bin/sh").is_err());
        assert!(validate_git_ref("--upload-pack=/bin/sh").is_err());
    }

    #[test]
    fn git_ref_rejects_control_and_size() {
        assert!(validate_git_ref("").is_err());
        assert!(validate_git_ref("a\nb").is_err());
        assert!(validate_git_ref("a\0b").is_err());
        let long = "a".repeat(201);
        assert!(validate_git_ref(&long).is_err());
    }
}
