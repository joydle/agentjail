//! veth pair + allowlist CONNECT-proxy orchestration.
//!
//! Split out from [`crate::run`] so the jail lifecycle file stays
//! focused on process control. Everything here is host-side plumbing
//! that runs *before* the child enters its network namespace: pick a
//! unique veth id, derive the IP pair, spawn the proxy thread, wire
//! the env vars the jail sees.
//!
//! # Lifecycle (what `run.rs` should do)
//!
//! ```ignore
//! // 1. Reserve an id + addresses. Nothing exists on the host yet.
//! let mut veth = VethPair::allocate()?;
//! // 2. Create the pair. From here on, dropping `veth` deletes it.
//! veth.create()?;
//! // 3. Move the jail end into the child's netns, address + up the host end.
//! veth.attach(child_pid.as_raw())?;
//! // 4. Host firewall for aj-h* (once per process; see `firewall`).
//! ensure_firewall(config.firewall)?;
//! // 5. Proxy pinned to this jail's address; Err if it can't bind.
//! let proxy_shutdown = spawn_proxy(domains.clone(), veth.host_ip(), veth.jail_ip(), ports)?;
//! // 6. Release the child past the netns handshake. It already knows its
//! //    interface name and addresses: `run.rs` allocates the pair before
//! //    fork so the proxy URL can go into the jail's environment.
//! // 7. Keep `veth` in the JailHandle; dropping it (cleanup_veth / Drop)
//! //    deletes the link and returns the id to the allocator.
//! ```
//!
//! Any `?` between steps 2 and 7 drops `veth`, which removes `aj-hN`
//! (and with it the peer) instead of leaking it.
//!
//! The pair is also torn down automatically when the child dies (the
//! child holds the netns; kernel removes the veth on ns drop). See
//! [`cleanup_stale_veths`] for the bug-belt that sweeps leftovers.

mod firewall;

pub(crate) use firewall::ensure as ensure_firewall;

use crate::error::{JailError, Result};
use crate::netlink;
use crate::proxy::{self, ProxyConfig};
use std::collections::BTreeSet;
#[cfg(test)]
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Mutex;

pub(crate) const PROXY_PORT: u16 = 8080;

/// Highest veth id. [`veth_addrs`] uses 16 bits of the id, so ids
/// `1..=MAX_VETH_ID` map to distinct `/30`s inside `10.0.0.0/8`.
pub(crate) const MAX_VETH_ID: u32 = 0xFFFF;

/// Derive host/jail IP addresses from a veth ID.
///
/// Injective on `1..=MAX_VETH_ID`: id `N` → `10.(N>>8).(N&0xff).{1,2}`.
pub(crate) fn veth_addrs(id: u32) -> (Ipv4Addr, Ipv4Addr) {
    let b2 = ((id >> 8) & 0xFF) as u8;
    let b3 = (id & 0xFF) as u8;
    let b3 = if b2 == 0 && b3 == 0 { 1 } else { b3 };
    (Ipv4Addr::new(10, b2, b3, 1), Ipv4Addr::new(10, b2, b3, 2))
}

/// Jail-side address of the `/30` whose host side is `host_ip`.
#[cfg(test)]
fn jail_ip_for_host(host_ip: Ipv4Addr) -> Ipv4Addr {
    let [a, b, c, d] = host_ip.octets();
    Ipv4Addr::new(a, b, c, d.wrapping_add(1))
}

// ---------------- id allocator ----------------

/// Process-wide veth id allocator: a free-list of released ids plus a
/// high-water mark. An id is handed out only if it is neither in use by
/// this process nor present on the host as `aj-h<id>` (another
/// agentjail process, or a leftover).
#[derive(Debug)]
pub(crate) struct IdAllocator {
    next: u32,
    max: u32,
    free: BTreeSet<u32>,
    in_use: BTreeSet<u32>,
}

impl IdAllocator {
    pub(crate) const fn new(max: u32) -> Self {
        Self {
            next: 1,
            max,
            free: BTreeSet::new(),
            in_use: BTreeSet::new(),
        }
    }

    /// Reserve an id; `taken(id)` reports ids that are busy outside this
    /// allocator. Returns `None` when every id is in use.
    pub(crate) fn alloc(&mut self, taken: impl Fn(u32) -> bool) -> Option<u32> {
        let mut skipped = Vec::new();
        let mut got = None;
        while let Some(id) = self.free.pop_first() {
            if taken(id) {
                skipped.push(id);
            } else {
                got = Some(id);
                break;
            }
        }
        while got.is_none() && self.next <= self.max {
            let id = self.next;
            self.next += 1;
            if taken(id) {
                skipped.push(id);
            } else {
                got = Some(id);
            }
        }
        // Externally-busy ids go back on the free list to be retried later.
        self.free.extend(skipped);
        if let Some(id) = got {
            self.in_use.insert(id);
        }
        got
    }

    /// Return an id. Releasing an id that isn't in use is a no-op, so a
    /// double release can never put a live id on the free list.
    pub(crate) fn release(&mut self, id: u32) {
        if self.in_use.remove(&id) {
            self.free.insert(id);
        }
    }
}

static ALLOCATOR: Mutex<IdAllocator> = Mutex::new(IdAllocator::new(MAX_VETH_ID));

fn host_iface_exists(id: u32) -> bool {
    std::path::Path::new(&format!("/sys/class/net/aj-h{id}")).exists()
        || std::path::Path::new(&format!("/sys/class/net/aj-j{id}")).exists()
}

fn lock_allocator() -> std::sync::MutexGuard<'static, IdAllocator> {
    ALLOCATOR.lock().unwrap_or_else(|p| p.into_inner())
}

// ---------------- RAII veth pair ----------------

/// A reserved veth id and, once [`create`](Self::create)d, the
/// `aj-h<id>` / `aj-j<id>` pair.
///
/// Dropping it deletes the host end (which removes the peer too, even if
/// it was already moved into the jail's netns) and releases the id. This
/// is both the setup-time guard (an error after `create` doesn't leak
/// `aj-hN`) and the long-lived owner the `JailHandle` keeps until
/// teardown: the id must stay reserved for as long as the link may exist,
/// so there is no separate "commit" step — keep the value alive to keep
/// the link, drop it to remove it.
#[derive(Debug)]
pub(crate) struct VethPair {
    id: u32,
    host_if: String,
    jail_if: String,
    host_ip: Ipv4Addr,
    jail_ip: Ipv4Addr,
    created: bool,
}

impl VethPair {
    /// Reserve a unique id and derive names/addresses. Creates nothing.
    pub(crate) fn allocate() -> Result<Self> {
        let id = lock_allocator().alloc(host_iface_exists).ok_or_else(|| {
            JailError::Network(std::io::Error::other(format!(
                "all {MAX_VETH_ID} veth ids are in use"
            )))
        })?;
        let (host_ip, jail_ip) = veth_addrs(id);
        Ok(Self {
            id,
            host_if: format!("aj-h{id}"),
            jail_if: format!("aj-j{id}"),
            host_ip,
            jail_ip,
            created: false,
        })
    }

    /// Create the veth pair in the host netns. Arms deletion-on-drop.
    pub(crate) fn create(&mut self) -> Result<()> {
        netlink::create_veth_pair(&self.host_if, &self.jail_if)?;
        self.created = true;
        Ok(())
    }

    /// Move the jail end into `child_pid`'s netns, then address and bring
    /// up the host end.
    pub(crate) fn attach(&mut self, child_pid: u32) -> Result<()> {
        netlink::move_to_netns(&self.jail_if, child_pid)?;
        netlink::add_ipv4_addr(&self.host_if, self.host_ip, 30)?;
        netlink::set_link_up(&self.host_if)?;
        Ok(())
    }

    /// The reserved id (`aj-h<id>` / `aj-j<id>`).
    #[cfg(test)]
    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    /// Host-side interface name (`aj-h<id>`).
    #[cfg(test)]
    pub(crate) fn host_if(&self) -> &str {
        &self.host_if
    }

    /// Jail-side interface name (`aj-j<id>`), moved into the jail's netns
    /// by [`attach`](Self::attach).
    pub(crate) fn jail_if(&self) -> &str {
        &self.jail_if
    }

    /// Host-side address (proxy bind address, jail's default gateway).
    pub(crate) fn host_ip(&self) -> Ipv4Addr {
        self.host_ip
    }

    /// Jail-side address.
    pub(crate) fn jail_ip(&self) -> Ipv4Addr {
        self.jail_ip
    }
}

impl Drop for VethPair {
    fn drop(&mut self) {
        if self.created {
            // ENODEV is expected when the netns already went away with
            // the child; nothing else to do in that case.
            let _ = netlink::delete_link(&self.host_if);
            // If the jail end was never moved, it's still here — but
            // deleting either end of a veth removes both, so this only
            // matters if host-end deletion failed for another reason.
            let _ = netlink::delete_link(&self.jail_if);
        }
        lock_allocator().release(self.id);
    }
}

// ---------------- proxy ----------------

/// Create a Unix socketpair for parent-child synchronization.
pub(crate) fn sync_socketpair() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    // SAFETY: socketpair with valid args, fds array is correctly sized.
    let ret = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if ret < 0 {
        return Err(JailError::Network(std::io::Error::last_os_error()));
    }
    // SAFETY: fds are valid, newly created by socketpair.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Proxy port allowlist for runtime-spawned proxies when
/// `JailConfig::proxy_allowed_ports` is `None`: from
/// [`proxy::ALLOWED_PORTS_ENV`] if set and valid, else `[443]`.
fn allowed_ports_from_env() -> Vec<u16> {
    match std::env::var(proxy::ALLOWED_PORTS_ENV) {
        Ok(v) => proxy::parse_port_list(&v).unwrap_or_else(|| {
            eprintln!(
                "warning: ignoring invalid {}={v:?}; using {:?}",
                proxy::ALLOWED_PORTS_ENV,
                proxy::DEFAULT_ALLOWED_PORTS
            );
            proxy::DEFAULT_ALLOWED_PORTS.to_vec()
        }),
        Err(_) => proxy::DEFAULT_ALLOWED_PORTS.to_vec(),
    }
}

/// Spawn the allowlist proxy for one jail in a background thread.
///
/// The proxy binds `host_ip:PROXY_PORT` and only serves connections whose
/// source address is `jail_ip`, so a neighbouring jail can't use this
/// jail's allowlist. Returns `Err` if the listener can't bind or the
/// thread dies before reporting readiness — the jail must not start with
/// `HTTP(S)_PROXY` pointing at nothing.
///
/// `allowed_ports` is the CONNECT destination-port allowlist; `None`
/// means `AGENTJAIL_PROXY_ALLOWED_PORTS`, else `[443]`.
///
/// Dropping (or sending on) the returned sender stops the proxy.
pub(crate) fn spawn_proxy(
    domains: Vec<String>,
    host_ip: Ipv4Addr,
    jail_ip: Ipv4Addr,
    allowed_ports: Option<&[u16]>,
) -> Result<tokio::sync::watch::Sender<bool>> {
    let config = ProxyConfig {
        allowlist: domains
            .iter()
            .map(|d| proxy::DomainPattern::parse(d))
            .collect(),
        port: PROXY_PORT,
        bind_ip: IpAddr::V4(host_ip),
        allowed_peer: Some(IpAddr::V4(jail_ip)),
        allowed_ports: allowed_ports.map_or_else(allowed_ports_from_env, <[u16]>::to_vec),
        ..ProxyConfig::default()
    };
    spawn_proxy_with(config)
}

/// [`spawn_proxy`] with a fully specified config.
pub(crate) fn spawn_proxy_with(config: ProxyConfig) -> Result<tokio::sync::watch::Sender<bool>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<std::result::Result<(), String>>(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let bind = format!("{}:{}", config.bind_ip, config.port);

    std::thread::Builder::new()
        .name("aj-proxy".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("proxy runtime: {e}")));
                    return;
                }
            };
            rt.block_on(async {
                if let Err(e) = proxy::run_proxy(config, tx, shutdown_rx).await {
                    eprintln!("proxy error: {e}");
                }
            });
        })
        .map_err(JailError::Network)?;

    match rx.recv() {
        Ok(Ok(())) => Ok(shutdown_tx),
        Ok(Err(e)) => Err(JailError::Network(std::io::Error::other(format!(
            "proxy bind {bind} failed: {e}"
        )))),
        Err(_) => Err(JailError::Network(std::io::Error::other(
            "proxy thread died before signaling readiness",
        ))),
    }
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
/// Normally veths are cleaned up by their [`VethPair`] owner (the
/// `JailHandle`), and if the embedder dies, `PR_SET_PDEATHSIG` kills
/// the jail, which destroys its network namespace and both veth ends.
/// This function handles the edge cases where neither ran (e.g. the
/// embedder was SIGKILLed while a link was being created).
///
/// Safe to call at any time — only touches interfaces whose name
/// starts with `aj-h` and whose id isn't reserved by a live
/// [`VethPair`] in this process.
pub fn cleanup_stale_veths() {
    let net_dir = std::path::Path::new("/sys/class/net");
    let entries = match std::fs::read_dir(net_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let live: BTreeSet<u32> = lock_allocator().in_use.clone();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(id) = name.strip_prefix("aj-h") {
            if id.parse::<u32>().is_ok_and(|id| live.contains(&id)) {
                continue;
            }
            let _ = netlink::delete_link(&name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn veth_addrs_injective_over_id_space() {
        let mut seen = HashSet::new();
        for id in 1..=MAX_VETH_ID {
            let (h, j) = veth_addrs(id);
            assert_eq!(jail_ip_for_host(h), j);
            assert!(seen.insert(h), "id {id} collides on {h}");
        }
    }

    #[test]
    fn allocator_never_hands_out_in_use_id() {
        let mut a = IdAllocator::new(MAX_VETH_ID);
        let mut live = HashSet::new();
        for _ in 0..MAX_VETH_ID {
            let id = a.alloc(|_| false).expect("id space not exhausted yet");
            assert!((1..=MAX_VETH_ID).contains(&id));
            assert!(live.insert(id), "duplicate id {id}");
        }
        // A wrapping counter would go 65 536 → 0 and collide with 1.
        assert_eq!(a.alloc(|_| false), None, "exhausted: must refuse, not wrap");

        // Freed ids are reused; still-live ones never are.
        a.release(7);
        a.release(42);
        a.release(42); // double release is harmless
        live.remove(&7);
        live.remove(&42);
        let x = a.alloc(|_| false).unwrap();
        let y = a.alloc(|_| false).unwrap();
        assert_eq!((x, y), (7, 42));
        assert_eq!(a.alloc(|_| false), None);
    }

    #[test]
    fn allocator_skips_ids_busy_on_host() {
        let mut a = IdAllocator::new(10);
        // Ids 1 and 2 exist on the host (another process).
        let first = a.alloc(|id| id <= 2).unwrap();
        assert_eq!(first, 3);
        // Once they disappear they become available.
        let next = a.alloc(|_| false).unwrap();
        assert_eq!(next, 1);
        // Releasing an id that was never handed out does nothing.
        a.release(9);
        assert_eq!(a.alloc(|_| false), Some(2));
        assert_eq!(a.alloc(|_| false), Some(4));
    }

    #[test]
    fn veth_pair_releases_id_on_drop() {
        let p = VethPair::allocate().unwrap();
        let id = p.id();
        assert!(lock_allocator().in_use.contains(&id));
        assert_eq!(p.host_if(), format!("aj-h{id}"));
        assert_eq!(veth_addrs(id), (p.host_ip(), p.jail_ip()));
        drop(p);
        assert!(!lock_allocator().in_use.contains(&id));
    }

    #[test]
    fn spawn_proxy_bind_failure_is_an_error() {
        // 192.0.2.1 (TEST-NET-1) is not a local address: bind must fail
        // with EADDRNOTAVAIL, and that must surface as Err.
        let err = spawn_proxy(
            vec![],
            Ipv4Addr::new(192, 0, 2, 1),
            Ipv4Addr::new(192, 0, 2, 2),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("proxy bind"), "{err}");
    }

    #[test]
    fn spawn_proxy_ok_and_stops_on_drop() {
        let tx = spawn_proxy_with(ProxyConfig {
            port: 0,
            ..ProxyConfig::default()
        })
        .unwrap();
        drop(tx);
    }
}
