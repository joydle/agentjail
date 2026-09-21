//! veth pair + allowlist CONNECT-proxy orchestration.
//!
//! Split out from [`crate::run`] so the jail lifecycle file stays
//! focused on process control. Everything here is host-side plumbing
//! that runs *before* the child enters its network namespace: pick a
//! unique veth id, derive the IP pair, spawn the proxy thread, wire
//! the env vars the jail sees.
//!
//! The pair is torn down automatically when the child dies (the child
//! holds the netns; kernel removes the veth on ns drop). See
//! [`cleanup_stale_veths`] for the bug-belt that sweeps leftovers.

use crate::error::{JailError, Result};
use crate::netlink;
use crate::proxy::{self, ProxyConfig};
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::atomic::AtomicU32;

pub(crate) const PROXY_PORT: u16 = 8080;

/// Monotonic counter for unique veth pair naming and IP addressing.
pub(crate) static NEXT_VETH_ID: AtomicU32 = AtomicU32::new(1);

/// Derive host/jail IP addresses from a veth ID.
pub(crate) fn veth_addrs(id: u32) -> (Ipv4Addr, Ipv4Addr) {
    let b2 = ((id >> 8) & 0xFF) as u8;
    let b3 = (id & 0xFF) as u8;
    let b3 = if b2 == 0 && b3 == 0 { 1 } else { b3 };
    (Ipv4Addr::new(10, b2, b3, 1), Ipv4Addr::new(10, b2, b3, 2))
}

/// Create a Unix socketpair for parent-child synchronization.
pub(crate) fn sync_socketpair() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    // SAFETY: socketpair with valid args, fds array is correctly sized.
    let ret = unsafe {
        libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr())
    };
    if ret < 0 {
        return Err(JailError::Network(std::io::Error::last_os_error()));
    }
    // SAFETY: fds are valid, newly created by socketpair.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Spawn the allowlist proxy in a background thread (parent process).
pub(crate) fn spawn_allowlist_proxy(
    domains: Vec<String>,
    bind_ip: Ipv4Addr,
) -> tokio::sync::watch::Sender<bool> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<std::result::Result<(), String>>(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("proxy runtime");

        let config = ProxyConfig {
            allowlist: domains.iter().map(|d| proxy::DomainPattern::parse(d)).collect(),
            port: PROXY_PORT,
            bind_ip: IpAddr::V4(bind_ip),
        };

        rt.block_on(async {
            if let Err(e) = proxy::run_proxy(config, tx, shutdown_rx).await {
                eprintln!("proxy error: {e}");
            }
        });
    });

    match rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("proxy bind failed: {e}"),
        Err(_) => eprintln!("proxy thread died before signaling readiness"),
    }

    shutdown_tx
}

/// Proxy environment variables for the jailed process.
pub(crate) fn proxy_env_vars(host_ip: Ipv4Addr) -> Vec<(String, String)> {
    let url = format!("http://{host_ip}:{PROXY_PORT}");
    vec![
        ("HTTP_PROXY".into(), url.clone()),
        ("HTTPS_PROXY".into(), url.clone()),
        ("http_proxy".into(), url.clone()),
        ("https_proxy".into(), url),
    ]
}

/// Remove leftover `aj-h*` veth interfaces from previous runs.
///
/// Normally veths are cleaned up automatically: `PR_SET_PDEATHSIG`
/// ensures the jailed child dies when the parent is killed, destroying
/// the network namespace and both veth ends. This function handles the
/// edge case where that mechanism failed (e.g. parent killed between
/// fork and prctl, or kernel bug).
///
/// Safe to call at any time — only touches interfaces whose name
/// starts with `aj-h`.
pub fn cleanup_stale_veths() {
    let net_dir = std::path::Path::new("/sys/class/net");
    let entries = match std::fs::read_dir(net_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("aj-h") {
            let _ = netlink::delete_link(&name);
        }
    }
}
