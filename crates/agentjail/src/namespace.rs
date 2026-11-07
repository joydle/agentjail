//! Linux namespace setup.
//!
//! Namespaces provide isolation:
//! - User: UID/GID mapping, rootless operation
//! - Mount: Private filesystem view
//! - PID: Isolated process tree
//! - Network: Isolated network stack
//! - IPC: Isolated shared memory/semaphores

use crate::error::{JailError, Result};
use rustix::process::Pid;
use rustix::thread::{UnshareFlags, unshare};
use std::fs;

/// Namespace configuration for a jail.
#[derive(Debug, Clone, Copy)]
pub struct NamespaceConfig {
    pub user: bool,
    pub mount: bool,
    pub pid: bool,
    pub network: bool,
    pub ipc: bool,
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        Self {
            user: true,
            mount: true,
            pid: true,
            network: true,
            ipc: true,
        }
    }
}

/// Enter new namespaces based on config.
pub fn enter_namespaces(config: NamespaceConfig) -> Result<()> {
    let mut flags = UnshareFlags::empty();

    if config.user {
        flags |= UnshareFlags::NEWUSER;
    }
    if config.mount {
        flags |= UnshareFlags::NEWNS;
    }
    if config.pid {
        flags |= UnshareFlags::NEWPID;
    }
    if config.network {
        flags |= UnshareFlags::NEWNET;
    }
    if config.ipc {
        flags |= UnshareFlags::NEWIPC;
    }

    unshare(flags).map_err(JailError::Namespace)?;

    Ok(())
}

/// Enter only a new user namespace. Paired with the parent writing
/// `setgroups` / `uid_map` / `gid_map` on our behalf — we signal
/// readiness, the parent writes, and we then proceed with the
/// remaining namespace unshares. Doing NEWUSER in isolation is the
/// only way to get the map write to land on the correct namespace:
/// if the child unshares `NEWUSER` *and* `NEWNS`/`NEWNET`/etc. all
/// at once after the parent's write, the write races past a
/// pre-unshare process and is effectively a no-op.
pub fn enter_user_namespace_alone() -> Result<()> {
    unshare(UnshareFlags::NEWUSER).map_err(JailError::Namespace)
}

/// Write UID/GID mappings for the child's user namespace.
///
/// Maps jail uid/gid 0 to `host_uid` / `host_gid` (a single id each).
/// The caller picks the host ids and must never pass 0: a root embedder
/// maps to an unprivileged id from the configured range, an unprivileged
/// embedder maps to its own uid/gid.
///
/// `setgroups` is set to `deny` first. That is mandatory for an
/// unprivileged writer, and for a root writer it stops the jail from
/// ever picking up supplementary groups (the child clears its groups
/// before unsharing).
///
/// Must be called from the parent after the child entered the user
/// namespace.
pub fn write_uid_gid_map(child_pid: Pid, host_uid: u32, host_gid: u32) -> Result<()> {
    if host_uid == 0 || host_gid == 0 {
        return Err(JailError::UidMap(std::io::Error::other(
            "refusing to map the jail's root to host uid/gid 0",
        )));
    }
    let pid = child_pid.as_raw_nonzero();

    fs::write(format!("/proc/{pid}/setgroups"), "deny").map_err(JailError::UidMap)?;
    fs::write(format!("/proc/{pid}/uid_map"), format!("0 {host_uid} 1\n"))
        .map_err(JailError::UidMap)?;
    fs::write(format!("/proc/{pid}/gid_map"), format!("0 {host_gid} 1\n"))
        .map_err(JailError::UidMap)?;

    Ok(())
}

/// Setup loopback interface in network namespace.
///
/// Uses ioctl(SIOCSIFFLAGS) directly instead of shelling out to `ip`,
/// so this works in minimal containers without iproute2.
pub fn setup_loopback() -> Result<()> {
    // SAFETY: Creating a UDP socket just for the ioctl. CLOEXEC prevents leak to exec'd child.
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if sock < 0 {
        return Err(JailError::Exec(std::io::Error::last_os_error()));
    }

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    // Write "lo\0" into ifr_name byte-by-byte to avoid c_char signedness issues.
    ifr.ifr_name[0] = b'l' as _;
    ifr.ifr_name[1] = b'o' as _;
    ifr.ifr_name[2] = 0;
    ifr.ifr_ifru.ifru_flags = (libc::IFF_UP | libc::IFF_LOOPBACK) as i16;

    // SAFETY: Valid socket fd, valid ifreq struct with "lo" interface name.
    let ret = unsafe { libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &ifr) };
    unsafe { libc::close(sock) };

    if ret < 0 {
        return Err(JailError::Exec(std::io::Error::last_os_error()));
    }

    Ok(())
}
