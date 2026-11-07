//! Error types for agentjail.

use std::io;

/// All errors that can occur during jail setup or execution.
#[derive(Debug, thiserror::Error)]
pub enum JailError {
    #[error("namespace setup failed: {0}")]
    Namespace(#[source] rustix::io::Errno),

    #[error("namespace uid/gid map failed: {0}")]
    UidMap(#[source] io::Error),

    #[error("mount failed: {0}")]
    Mount(#[source] rustix::io::Errno),

    #[error("filesystem setup failed: {0}")]
    Io(#[source] io::Error),

    #[error("seccomp filter failed: {0}")]
    Seccomp(String),

    #[error("cgroup setup failed: {0}")]
    Cgroup(#[source] io::Error),

    #[error("snapshot failed: {0}")]
    Snapshot(#[source] io::Error),

    #[error("GPU setup failed: {0}")]
    Gpu(#[source] io::Error),

    #[error("landlock setup failed: {0}")]
    Landlock(#[source] landlock::RulesetError),

    #[error("network setup failed: {0}")]
    Network(#[source] io::Error),

    #[error("failed to execute command: {0}")]
    Exec(#[source] io::Error),

    #[error("fork failed: {0}")]
    Fork(#[source] rustix::io::Errno),

    #[error("pipe creation failed: {0}")]
    Pipe(#[source] rustix::io::Errno),

    #[error("path does not exist: {0}")]
    PathNotFound(std::path::PathBuf),

    #[error("bad config: {0}")]
    BadConfig(String),

    /// A sandbox layer the config asked for is not available on this
    /// kernel (e.g. `landlock = true` without Landlock support).
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The jailed child died or stalled before finishing the setup
    /// handshake with the parent. Its stderr usually says which step
    /// failed.
    #[error("jail setup handshake failed: {0}")]
    Handshake(String),
}

pub type Result<T> = std::result::Result<T, JailError>;
