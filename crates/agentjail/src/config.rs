//! Jail configuration.

use crate::error::{JailError, Result};
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

/// Host packet filter for `Network::Allowlist` jails (see
/// [`JailConfig::firewall`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FirewallMode {
    /// Install the nftables table if `nft` is available; otherwise warn
    /// once and rely on the proxy's source-address check alone.
    #[default]
    Auto,
    /// Installing the table must succeed, or the spawn fails.
    Require,
    /// Never touch the host firewall (you isolate `aj-h*` yourself).
    Off,
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
///     memory_mb: Some(1024),
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

    /// Output directory (mounted read-write, `nosuid,nodev`, inside the
    /// jail at `/output`).
    ///
    /// When the embedder is root, the jail runs as an unprivileged host
    /// id (see [`user_namespace`](Self::user_namespace) and
    /// [`run_as`](Self::run_as)); every spawn re-owns this tree (and the
    /// source tree when `source_rw`) to that id so the jail can write it.
    pub output: PathBuf,

    /// When `true`, the source mount at `/workspace` is read-write.
    /// Defaults to `false` so one-shot runs preserve the safer split of
    /// "seed dir is immutable, artifacts go to /output".
    pub source_rw: bool,

    /// Network policy.
    pub network: Network,

    /// Seccomp filtering level.
    pub seccomp: SeccompLevel,

    /// Host firewall for `Network::Allowlist`: an nftables table
    /// (`inet agentjail`, installed once per process by shelling out to
    /// `nft`) that lets a jail reach only its own proxy port on the host
    /// and drops everything else from or routed via `aj-h*` interfaces.
    /// `None` (default) reads `AGENTJAIL_FIREWALL` (`auto` | `nft` |
    /// `off`), falling back to [`FirewallMode::Auto`].
    pub firewall: Option<FirewallMode>,

    /// Destination ports the allowlist proxy will `CONNECT` to.
    /// `None` (default) reads `AGENTJAIL_PROXY_ALLOWED_PORTS`
    /// (comma-separated), falling back to `[443]`. An empty list or port
    /// `0` is rejected by [`JailConfig::validate`].
    pub proxy_allowed_ports: Option<Vec<u16>>,

    /// Use Landlock for filesystem isolation (default `true`).
    ///
    /// Applied by the jail's init after `pivot_root`, over in-jail paths:
    /// read+exec on `/bin`, `/lib`, `/lib64`, `/usr` (and `/opt/flavors`),
    /// read on `/etc` and `/proc`, device read/write/ioctl on `/dev`,
    /// read/write on `/tmp`, read/write/exec on `/output`, and read+exec
    /// (read/write with `source_rw`) on `/workspace`. Everything else —
    /// including the jail's `/` itself — is denied. [`crate::Jail::new`]
    /// returns [`JailError::Unsupported`] when the kernel lacks Landlock.
    pub landlock: bool,

    /// Memory limit in megabytes. `None` = no memory limit. `Some(0)` is
    /// rejected by [`JailConfig::validate`] — "zero" never silently means
    /// "unlimited".
    pub memory_mb: Option<u64>,

    /// CPU quota as percentage (100 = one full core, 0 = unlimited).
    /// At most [`MAX_CPU_PERCENT`].
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

    /// Use a user namespace.
    ///
    /// The jail's uid/gid 0 never maps to host uid/gid 0:
    /// - unprivileged embedder: jail 0 maps to the embedder's own uid/gid;
    /// - root embedder: jail 0 maps to an unprivileged host id taken from
    ///   [`userns_id_base`](Self::userns_id_base) /
    ///   [`userns_id_count`](Self::userns_id_count).
    pub user_namespace: bool,

    /// First host uid/gid used for the jail's root when the embedder is
    /// real root and `user_namespace = true`. Default `100_000`.
    pub userns_id_base: u32,

    /// Number of host ids, starting at `userns_id_base`, that spawns
    /// rotate through (one id per spawn). Default `1`: every jail's root
    /// is host id `100_000`, so a persistent output directory stays
    /// writable across spawns. Raise it to give concurrent jails distinct
    /// host ids (each spawn then owns only the files it creates).
    pub userns_id_count: u32,

    /// Host uid/gid the jailed process runs as when the embedder is real
    /// root and `user_namespace = false`. Default `65534:65534` (nobody).
    /// Uid or gid `0` is rejected. Ignored for unprivileged embedders
    /// (the jail already runs as the embedder's uid).
    pub run_as: RunAs,

    /// Inherit the embedder's stdin. Default `false`: the jail's stdin is
    /// `/dev/null`.
    pub inherit_stdin: bool,

    /// Use a PID namespace (isolated process tree). Must be `true`;
    /// [`validate`](Self::validate) rejects `false`.
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
            firewall: None,
            proxy_allowed_ports: None,
            landlock: true,
            memory_mb: Some(512),
            cpu_percent: 100,
            max_pids: 64,
            io_read_mbps: 0,
            io_write_mbps: 0,
            timeout_secs: 300,
            env: Vec::new(),
            user_namespace: true,
            userns_id_base: DEFAULT_USERNS_ID_BASE,
            userns_id_count: 1,
            run_as: RunAs::NOBODY,
            inherit_stdin: false,
            pid_namespace: true,
            ipc_namespace: true,
            workdir: PathBuf::from("/workspace"),
            gpu: GpuConfig::default(),
            readonly_overlays: Vec::new(),
        }
    }
}

/// Default first host id for root-embedder user namespaces.
pub const DEFAULT_USERNS_ID_BASE: u32 = 100_000;

/// Upper bound for [`JailConfig::cpu_percent`] (1000 cores).
pub const MAX_CPU_PERCENT: u64 = 100_000;

/// Host uid/gid pair for [`JailConfig::run_as`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunAs {
    pub uid: u32,
    pub gid: u32,
}

impl RunAs {
    /// `nobody:nogroup` (65534:65534).
    pub const NOBODY: RunAs = RunAs {
        uid: 65534,
        gid: 65534,
    };
}

impl Default for RunAs {
    fn default() -> Self {
        Self::NOBODY
    }
}

const MIB: u64 = 1024 * 1024;

impl JailConfig {
    /// Memory limit in bytes, `None` when unlimited.
    pub(crate) fn memory_limit_bytes(&self) -> Result<Option<u64>> {
        match self.memory_mb {
            None => Ok(None),
            Some(0) => Err(JailError::BadConfig(
                "memory_mb = Some(0) is not a usable limit; use None for no memory limit".into(),
            )),
            Some(mb) => mb.checked_mul(MIB).map(Some).ok_or_else(|| {
                JailError::BadConfig(format!("memory_mb = {mb} overflows a byte count"))
            }),
        }
    }

    /// Disk read / write limits in bytes per second (`0` = unlimited).
    pub(crate) fn io_limit_bps(&self) -> Result<(u64, u64)> {
        let conv = |name: &str, mbps: u64| {
            mbps.checked_mul(MIB).ok_or_else(|| {
                JailError::BadConfig(format!("{name} = {mbps} overflows a byte rate"))
            })
        };
        Ok((
            conv("io_read_mbps", self.io_read_mbps)?,
            conv("io_write_mbps", self.io_write_mbps)?,
        ))
    }

    /// Reject configurations that would overflow, silently weaken the
    /// sandbox, or cannot work. Called by [`crate::Jail::new`].
    pub fn validate(&self) -> Result<()> {
        self.memory_limit_bytes()?;
        self.io_limit_bps()?;
        if self.cpu_percent > MAX_CPU_PERCENT {
            return Err(JailError::BadConfig(format!(
                "cpu_percent = {} exceeds the maximum of {MAX_CPU_PERCENT}",
                self.cpu_percent
            )));
        }
        if !self.pid_namespace {
            // With a user namespace, procfs must come from a pid namespace
            // the jail owns. Without one, the jail would see and could
            // signal every host process its uid may signal.
            return Err(JailError::BadConfig(
                "pid_namespace = false is not supported: the jail would share the \
                 host's pid namespace"
                    .into(),
            ));
        }
        if self.userns_id_base == 0 {
            return Err(JailError::BadConfig(
                "userns_id_base = 0 would map the jail's root to host root".into(),
            ));
        }
        if self.userns_id_count == 0 {
            return Err(JailError::BadConfig(
                "userns_id_count must be at least 1".into(),
            ));
        }
        if self
            .userns_id_base
            .checked_add(self.userns_id_count - 1)
            .is_none_or(|last| last == u32::MAX)
        {
            return Err(JailError::BadConfig(
                "userns_id_base + userns_id_count overflows the uid space".into(),
            ));
        }
        if let Some(ports) = &self.proxy_allowed_ports
            && (ports.is_empty() || ports.contains(&0))
        {
            return Err(JailError::BadConfig(
                "proxy_allowed_ports must be non-empty and must not contain port 0; \
                 use None for the default ([443])"
                    .into(),
            ));
        }
        if self.run_as.uid == 0 || self.run_as.gid == 0 {
            return Err(JailError::BadConfig(
                "run_as must not be uid/gid 0 (the jail would run as host root)".into(),
            ));
        }
        Ok(())
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
        memory_mb: Some(512),
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
        memory_mb: Some(512),
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
        memory_mb: Some(256),
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
        memory_mb: Some(8192),
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
        memory_mb: Some(1024),
        cpu_percent: 400,
        max_pids: 256,
        timeout_secs: 3600,
        ..Default::default()
    }
}
