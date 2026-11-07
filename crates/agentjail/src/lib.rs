//! # agentjail
//!
//! Minimal Linux sandbox for running untrusted code.
//!
//! ## Quick Start
//!
//! ```ignore
//! use agentjail::{Jail, JailConfig, preset_build};
//!
//! // Use a preset
//! let config = preset_build("/path/to/source", "/path/to/output");
//! let jail = Jail::new(config)?;
//! let output = jail.run("npm", &["run", "build"]).await?;
//!
//! // Or configure manually
//! let config = JailConfig {
//!     source: "/code".into(),
//!     output: "/artifacts".into(),
//!     memory_mb: Some(1024),
//!     timeout_secs: 60,
//!     ..Default::default()
//! };
//! let jail = Jail::new(config)?;
//! ```
//!
//! ## Security Layers
//!
//! The jail provides defense in depth:
//!
//! 1. **Namespaces** - Isolated process, mount, network, and user views
//! 2. **Seccomp** - Syscall filtering to block dangerous operations
//! 3. **Cgroups** - Resource limits (memory, CPU, PIDs)
//! 4. **Landlock** - Filesystem access control
//!
//! The jailed process never runs as host uid 0: with a user namespace the
//! jail's root maps to an unprivileged host id, and without one the
//! process drops to [`JailConfig::run_as`] (nobody by default) when the
//! embedder is root. `/proc` is masked like runc's default (`/proc/sys`,
//! `/proc/sysrq-trigger`, … read-only; `/proc/kcore`, `/proc/keys`, …
//! hidden).
//!
//! ## Output
//!
//! - **stdout/stderr**: Captured as streams, available during execution
//! - **Artifacts**: Written to the output directory via bind mount

mod cgroup;
mod config;
mod error;
pub mod events;
mod exec;
mod fork;
mod gpu;
mod landlock;
mod mount;
mod namespace;
mod netlink;
mod pipe;
mod proxy;
mod run;
mod run_internal;
mod seccomp;
mod snapshot;
mod spawner;
mod veth;

// Public API
pub use config::{
    Access, DEFAULT_USERNS_ID_BASE, FirewallMode, GpuConfig, JailConfig, MAX_CPU_PERCENT, Network,
    RunAs, SeccompLevel, preset_agent, preset_build, preset_dev, preset_gpu, preset_install,
};
pub use error::{JailError, Result};
pub use events::{EventReceiver, EventSender, JailEvent};
pub use fork::{CloneMethod, ForkInfo};
pub use proxy::DomainPattern;
pub use run::{Jail, JailHandle, JailPid, Output, ResourceStats};
pub use snapshot::{
    Manifest, ManifestEntry, Snapshot, freeze_cgroup, gc_objects_pool, gc_objects_pool_locked,
    load_manifest, snapshot_frozen, thaw_cgroup,
};
pub use veth::cleanup_stale_veths;
