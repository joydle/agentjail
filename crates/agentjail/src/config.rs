//! Jail configuration.

use std::path::PathBuf;

/// Filesystem access level for mounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    #[default]
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

/// Network access policy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Network {
    /// No network access at all.
    #[default]
    None,
    /// Loopback (127.0.0.1) only.
    Loopback,
    /// Allowlist: only connect to specified domains via built-in proxy.
    /// Proxy runs on localhost, DNS resolved at connection time.
    Allowlist(Vec<String>),
}

/// Seccomp filter strictness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeccompLevel {
    /// No seccomp filtering.
    Disabled,
    /// Allow most syscalls, block dangerous ones (ptrace, reboot, etc).
    #[default]
    Standard,
    /// Minimal syscalls for builds (no socket creation, etc).
    Strict,
}

/// GPU passthrough configuration.
#[derive(Debug, Clone, Default)]
pub struct GpuConfig {
    /// Enable NVIDIA GPU passthrough.
    ///
    /// Mounts `/dev/nvidia*` devices and host NVIDIA libraries into the jail.
    /// This widens the attack surface — only enable for trusted workloads.
    pub enabled: bool,
    /// Specific GPU indices to expose (e.g., `vec![0]` for GPU 0 only).
    /// Empty means all available GPUs.
    pub devices: Vec<u32>,
}

/// Configuration for a jail instance.
///
/// Use struct syntax with `..Default::default()` for ergonomic construction:
///
/// ```ignore
/// let config = JailConfig {
///     source: "/path/to/code".into(),
///     output: "/path/to/artifacts".into(),
///     memory_mb: 1024,
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct JailConfig {
    /// Source directory (mounted at `/workspace`). Read-only by default
    /// — flip [`source_rw`](Self::source_rw) to `true` for persistent
    /// workspaces where the jail is expected to mutate its own tree
    /// (install deps, run builds, etc.).
    pub source: PathBuf,

    /// Output directory (mounted read-write inside jail at `/output`).
    pub output: PathBuf,

    /// When `true`, the source mount at `/workspace` is read-write.
    /// Defaults to `false` so one-shot runs preserve the safer split of
    /// "seed dir is immutable, artifacts go to /output".
    pub source_rw: bool,

    /// Network policy.
    pub network: Network,

    /// Seccomp filtering level.
    pub seccomp: SeccompLevel,

    /// Use landlock for filesystem isolation.
    pub landlock: bool,

    /// Memory limit in megabytes.
    pub memory_mb: u64,

    /// CPU quota as percentage (100 = one full core).
    pub cpu_percent: u64,

    /// Maximum number of processes/threads.
    pub max_pids: u64,

    /// Disk read bandwidth limit in MB/s (0 = unlimited).
    pub io_read_mbps: u64,

    /// Disk write bandwidth limit in MB/s (0 = unlimited).
    pub io_write_mbps: u64,

    /// Execution timeout in seconds (0 = no limit).
    pub timeout_secs: u64,

    /// Environment variables to pass through (empty = clean env).
    pub env: Vec<(String, String)>,

    /// Use user namespace (rootless mode).
    pub user_namespace: bool,

    /// Use PID namespace (isolated process tree).
    pub pid_namespace: bool,

    /// Use IPC namespace (isolated shared memory).
    pub ipc_namespace: bool,

    /// Working directory inside the jail.
    pub workdir: PathBuf,

    /// GPU passthrough.
    pub gpu: GpuConfig,

    /// Read-only host directories to bind-mount into the jail under
    /// `/opt/flavors/<basename>/`. Used to layer in runtime "flavors"
    /// (nodejs, python, bun, ...) without the engine needing to know
    /// anything about what's inside — each overlay is just a dir.
    ///
    /// The jail's `PATH` is extended automatically with
    /// `/opt/flavors/<basename>/bin` when that subdirectory exists, so
    /// callers don't have to thread flavor-specific `env` entries through.
    ///
    /// Empty by default. Duplicate basenames are a `BadConfig` error so
    /// the caller doesn't silently lose a flavor.
    pub readonly_overlays: Vec<PathBuf>,
}

impl Default for JailConfig {
    fn default() -> Self {
        Self {
            source: PathBuf::new(),
            output: PathBuf::new(),
            source_rw: false,
            network: Network::None,
            seccomp: SeccompLevel::Standard,
            landlock: true,
            memory_mb: 512,
            cpu_percent: 100,
            max_pids: 64,
            io_read_mbps: 0,
            io_write_mbps: 0,
            timeout_secs: 300,
            env: Vec::new(),
            user_namespace: true,
            pid_namespace: true,
            ipc_namespace: true,
            workdir: PathBuf::from("/workspace"),
            gpu: GpuConfig::default(),
            readonly_overlays: Vec::new(),
        }
    }
}

/// Preset for offline builds (vendored deps, no network).
///
/// Use this when all dependencies are already present (e.g. after npm ci
/// with a lockfile, cargo build with vendored crates). Seccomp Strict
/// blocks socket creation for maximum isolation.
pub fn preset_build(source: impl Into<PathBuf>, output: impl Into<PathBuf>) -> JailConfig {
    JailConfig {
        source: source.into(),
        output: output.into(),
        network: Network::None,
        seccomp: SeccompLevel::Strict,
        memory_mb: 512,
        cpu_percent: 400,
        max_pids: 128,
        timeout_secs: 600,
        ..Default::default()
    }
}

/// Preset for builds that need to fetch dependencies (npm install, cargo build).
///
/// Uses an allowlist proxy so only specified registries are reachable.
/// Caller must provide the domains to allow.
pub fn preset_install(
    source: impl Into<PathBuf>,
    output: impl Into<PathBuf>,
    allowed_domains: Vec<String>,
) -> JailConfig {
    JailConfig {
        source: source.into(),
        output: output.into(),
        network: Network::Allowlist(allowed_domains),
        seccomp: SeccompLevel::Standard,
        memory_mb: 512,
        cpu_percent: 400,
        max_pids: 128,
        timeout_secs: 600,
        ..Default::default()
    }
}

/// Preset for AI agent execution.
///
/// No network by default. For agents that need API access, set
/// `network: Network::Allowlist(vec!["api.anthropic.com".into()])`.
pub fn preset_agent(source: impl Into<PathBuf>, output: impl Into<PathBuf>) -> JailConfig {
    JailConfig {
        source: source.into(),
        output: output.into(),
        network: Network::None,
        seccomp: SeccompLevel::Standard,
        memory_mb: 256,
        cpu_percent: 100,
        max_pids: 32,
        timeout_secs: 300,
        ..Default::default()
    }
}

/// Preset for GPU/ML workloads (CUDA, PyTorch).
///
/// Mounts NVIDIA devices and libraries. Higher resource limits for
/// training jobs. No network by default — set an allowlist if the
/// workload needs to download models.
pub fn preset_gpu(source: impl Into<PathBuf>, output: impl Into<PathBuf>) -> JailConfig {
    JailConfig {
        source: source.into(),
        output: output.into(),
        network: Network::None,
        seccomp: SeccompLevel::Standard,
        memory_mb: 8192,
        cpu_percent: 400,
        max_pids: 256,
        timeout_secs: 3600,
        gpu: GpuConfig {
            enabled: true,
            devices: vec![],
        },
        ..Default::default()
    }
}

/// Preset for dev servers (HMR, watch mode).
pub fn preset_dev(source: impl Into<PathBuf>, output: impl Into<PathBuf>) -> JailConfig {
    JailConfig {
        source: source.into(),
        output: output.into(),
        network: Network::Loopback,
        seccomp: SeccompLevel::Standard,
        memory_mb: 1024,
        cpu_percent: 400,
        max_pids: 256,
        timeout_secs: 3600,
        ..Default::default()
    }
}
