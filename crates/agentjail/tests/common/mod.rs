//! Shared test helpers for agentjail integration tests.

use agentjail::{JailConfig, Network, SeccompLevel};
use std::fs;
use std::path::PathBuf;

/// Create temp directories for a test. Returns (source, output).
pub fn setup(prefix: &str, name: &str) -> (PathBuf, PathBuf) {
    let src = PathBuf::from(format!("/tmp/aj-{prefix}-{name}-src"));
    let out = PathBuf::from(format!("/tmp/aj-{prefix}-{name}-out"));
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&out).unwrap();
    (src, out)
}

/// Clean up temp directories.
pub fn cleanup(src: &PathBuf, out: &PathBuf) {
    let _ = fs::remove_dir_all(src);
    let _ = fs::remove_dir_all(out);
}

/// Lightweight config: no user namespace, no seccomp, no landlock.
/// Works in any environment (Docker, CI, etc).
pub fn lightweight_config(src: PathBuf, out: PathBuf) -> JailConfig {
    JailConfig {
        source: src,
        output: out,
        timeout_secs: 10,
        user_namespace: false,
        seccomp: SeccompLevel::Disabled,
        landlock: false,
        memory_mb: 0,
        cpu_percent: 0,
        max_pids: 0,
        pid_namespace: true,
        ..Default::default()
    }
}

/// Full sandbox config with seccomp, namespaces, etc.
/// Uses user_namespace when not root.
pub fn full_sandbox_config(src: PathBuf, out: PathBuf) -> JailConfig {
    let is_root = is_root();
    JailConfig {
        source: src,
        output: out,
        user_namespace: !is_root,
        pid_namespace: true,
        ipc_namespace: true,
        network: Network::None,
        seccomp: SeccompLevel::Standard,
        landlock: false,
        timeout_secs: 15,
        memory_mb: 0,
        cpu_percent: 0,
        max_pids: 0,
        env: vec![("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into())],
        ..Default::default()
    }
}

pub fn is_root() -> bool {
    unsafe { libc::getuid() == 0 }
}
