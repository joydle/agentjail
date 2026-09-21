//! Raw netlink operations for network interface configuration.
//!
//! Provides veth pair creation, IP assignment, and routing via
//! NETLINK_ROUTE sockets. No external tools required.

use crate::error::{JailError, Result};
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

// --- Netlink constants (not exported by libc on linux target) -----------

const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLM_F_EXCL: u16 = 0x200;
const NLM_F_CREATE: u16 = 0x400;
const NLA_F_NESTED: u16 = 1 << 15;
const NLMSG_ERROR: u16 = 2;

// if_link.h
const IFLA_INFO_KIND: u16 = 1;
const IFLA_INFO_DATA: u16 = 2;
const VETH_INFO_PEER: u16 = 1;

// --- Structs not in libc for linux target --------------------------------

#[repr(C)]
struct Ifaddrmsg {
    ifa_family: u8,
    ifa_prefixlen: u8,
    ifa_flags: u8,
    ifa_scope: u8,
    ifa_index: u32,
}

#[repr(C)]
struct Rtmsg {
    rtm_family: u8,
    rtm_dst_len: u8,
    rtm_src_len: u8,
    rtm_tos: u8,
    rtm_table: u8,
    rtm_protocol: u8,
    rtm_scope: u8,
    rtm_type: u8,
    rtm_flags: u32,
}

// --- Netlink helpers -----------------------------------------------------

/// Open a NETLINK_ROUTE socket.
fn nl_socket() -> std::io::Result<OwnedFd> {
    // SAFETY: Creating a netlink socket with valid constants.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Bind to kernel (pid=0, groups=0)
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as u16;
    // SAFETY: Valid socket fd and properly initialized sockaddr_nl.
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as u32,
        )
    };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    // SAFETY: fd is a valid, newly created socket.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Send a netlink message and wait for the kernel ACK.
fn nl_exec(sock: &OwnedFd, msg: &[u8]) -> std::io::Result<()> {
    let fd = sock.as_raw_fd();

    // SAFETY: Sending bytes on a valid netlink socket.
    let sent = unsafe { libc::send(fd, msg.as_ptr() as *const _, msg.len(), 0) };
    if sent < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Read ACK — use an aligned buffer to avoid UB when casting to nlmsghdr.
    #[repr(C, align(4))]
    struct AlignedBuf([u8; 1024]);
    let mut aligned = AlignedBuf([0u8; 1024]);
    let buf = &mut aligned.0;
    // SAFETY: Reading into a stack buffer from a valid socket.
    let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut _, buf.len(), 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Parse nlmsghdr + nlmsgerr
    if (n as usize) < std::mem::size_of::<libc::nlmsghdr>() + 4 {
        return Err(std::io::Error::other("netlink: short ACK"));
    }

    // SAFETY: Buffer is 4-byte aligned (AlignedBuf), we have enough bytes.
    let hdr = unsafe { &*(buf.as_ptr() as *const libc::nlmsghdr) };
    if hdr.nlmsg_type == NLMSG_ERROR {
        // Error code is a 4-byte i32 right after the header.
        let err_offset = std::mem::size_of::<libc::nlmsghdr>();
        let code = i32::from_ne_bytes(
            buf[err_offset..err_offset + 4]
                .try_into()
                .map_err(|_| std::io::Error::other("netlink: malformed ACK"))?,
        );
        if code < 0 {
            return Err(std::io::Error::from_raw_os_error(-code));
        }
        // code == 0 means ACK (success)
    }

    Ok(())
}

/// Append an rtattr to a buffer with 4-byte alignment.
fn push_attr(buf: &mut Vec<u8>, rta_type: u16, data: &[u8]) {
    let hdr_len = 4; // sizeof(rtattr) = 4 bytes (rta_len: u16 + rta_type: u16)
    let rta_len = (hdr_len + data.len()) as u16;
    buf.extend_from_slice(&rta_len.to_ne_bytes());
    buf.extend_from_slice(&rta_type.to_ne_bytes());
    buf.extend_from_slice(data);
    // Pad to 4-byte alignment
    let padded = ((rta_len as usize) + 3) & !3;
    buf.resize(buf.len() + padded - rta_len as usize, 0);
}

/// Append a nested rtattr (sets NLA_F_NESTED flag).
fn push_attr_nested(buf: &mut Vec<u8>, rta_type: u16, data: &[u8]) {
    push_attr(buf, rta_type | NLA_F_NESTED, data);
}

/// Maximum interface name length (including null terminator).
const IFNAMSIZ: usize = 16;

/// Look up interface index by name via ioctl.
fn ifindex(name: &str) -> std::io::Result<u32> {
    validate_ifname(name)?;

    // SAFETY: Creating a UDP socket for ioctl only. CLOEXEC prevents leak to children.
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if sock < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    write_ifr_name(&mut ifr, name);

    // SAFETY: Valid socket and properly initialized ifreq.
    let ret = unsafe { libc::ioctl(sock, libc::SIOCGIFINDEX as _, &ifr) };
    // Capture errno before close() can clobber it.
    let err = std::io::Error::last_os_error();
    unsafe { libc::close(sock) };

    if ret < 0 {
        return Err(err);
    }

    // SAFETY: ioctl succeeded, ifr_ifindex is populated.
    Ok(unsafe { ifr.ifr_ifru.ifru_ifindex } as u32)
}

/// Null-terminated name bytes for netlink attributes.
fn name_nul(name: &str) -> Vec<u8> {
    let mut v = name.as_bytes().to_vec();
    v.push(0);
    v
}

/// Validate interface name fits in IFNAMSIZ (16 bytes including null).
fn validate_ifname(name: &str) -> std::io::Result<()> {
    if name.is_empty() || name.len() >= IFNAMSIZ {
        return Err(std::io::Error::other(format!(
            "interface name must be 1..{} bytes, got {}",
            IFNAMSIZ - 1,
            name.len()
        )));
    }
    Ok(())
}

/// Write interface name into ifreq.ifr_name with bounds check.
fn write_ifr_name(ifr: &mut libc::ifreq, name: &str) {
    // Caller must validate via validate_ifname() first. The zeroed ifr
    // guarantees null termination as long as name.len() < IFNAMSIZ.
    for (dst, src) in ifr.ifr_name.iter_mut().zip(name.as_bytes()) {
        *dst = *src as _;
    }
}

/// Cast a struct to a byte slice.
///
/// # Safety
/// T must be repr(C) and contain no padding that leaks data.
unsafe fn as_bytes<T: Sized>(val: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(val as *const T as *const u8, std::mem::size_of::<T>()) }
}

// --- Public API -----------------------------------------------------------

/// Create a veth pair: `host_name` in current netns, `jail_name` as its peer.
pub fn create_veth_pair(host_name: &str, jail_name: &str) -> Result<()> {
    let sock = nl_socket().map_err(JailError::Network)?;

    // Build the peer's nested section: ifinfomsg + IFLA_IFNAME
    let mut peer_data = Vec::new();
    let peer_ifinfo: libc::ifinfomsg = unsafe { std::mem::zeroed() };
    peer_data.extend_from_slice(unsafe { as_bytes(&peer_ifinfo) });
    push_attr(&mut peer_data, libc::IFLA_IFNAME, &name_nul(jail_name));

    // IFLA_INFO_DATA containing VETH_INFO_PEER
    let mut info_data = Vec::new();
    push_attr_nested(&mut info_data, VETH_INFO_PEER, &peer_data);

    // IFLA_LINKINFO containing kind + data
    let mut linkinfo = Vec::new();
    push_attr(&mut linkinfo, IFLA_INFO_KIND, b"veth\0");
    push_attr_nested(&mut linkinfo, IFLA_INFO_DATA, &info_data);

    // Top-level payload: ifinfomsg + IFLA_IFNAME + IFLA_LINKINFO
    let mut payload = Vec::new();
    let ifinfo: libc::ifinfomsg = unsafe { std::mem::zeroed() };
    payload.extend_from_slice(unsafe { as_bytes(&ifinfo) });
    push_attr(&mut payload, libc::IFLA_IFNAME, &name_nul(host_name));
    push_attr_nested(&mut payload, libc::IFLA_LINKINFO, &linkinfo);

    // Assemble full message: nlmsghdr + payload
    let total_len = std::mem::size_of::<libc::nlmsghdr>() + payload.len();
    let hdr = libc::nlmsghdr {
        nlmsg_len: total_len as u32,
        nlmsg_type: libc::RTM_NEWLINK,
        nlmsg_flags: NLM_F_REQUEST | NLM_F_CREATE | NLM_F_EXCL | NLM_F_ACK,
        nlmsg_seq: 1,
        nlmsg_pid: 0,
    };

    let mut msg = Vec::with_capacity(total_len);
    msg.extend_from_slice(unsafe { as_bytes(&hdr) });
    msg.extend_from_slice(&payload);

    nl_exec(&sock, &msg).map_err(JailError::Network)
}

/// Move an interface into another network namespace by PID.
pub fn move_to_netns(ifname: &str, pid: u32) -> Result<()> {
    let sock = nl_socket().map_err(JailError::Network)?;
    let idx = ifindex(ifname).map_err(JailError::Network)?;

    let mut payload = Vec::new();
    let mut ifinfo: libc::ifinfomsg = unsafe { std::mem::zeroed() };
    ifinfo.ifi_index = idx as i32;
    payload.extend_from_slice(unsafe { as_bytes(&ifinfo) });
    push_attr(&mut payload, libc::IFLA_NET_NS_PID, &pid.to_ne_bytes());

    let total_len = std::mem::size_of::<libc::nlmsghdr>() + payload.len();
    let hdr = libc::nlmsghdr {
        nlmsg_len: total_len as u32,
        nlmsg_type: libc::RTM_NEWLINK,
        nlmsg_flags: NLM_F_REQUEST | NLM_F_ACK,
        nlmsg_seq: 2,
        nlmsg_pid: 0,
    };

    let mut msg = Vec::with_capacity(total_len);
    msg.extend_from_slice(unsafe { as_bytes(&hdr) });
    msg.extend_from_slice(&payload);

    nl_exec(&sock, &msg).map_err(JailError::Network)
}

/// Assign an IPv4 address to an interface.
pub fn add_ipv4_addr(ifname: &str, addr: Ipv4Addr, prefix_len: u8) -> Result<()> {
    let sock = nl_socket().map_err(JailError::Network)?;
    let idx = ifindex(ifname).map_err(JailError::Network)?;

    let mut payload = Vec::new();
    let ifa = Ifaddrmsg {
        ifa_family: libc::AF_INET as u8,
        ifa_prefixlen: prefix_len,
        ifa_flags: 0,
        ifa_scope: 0, // RT_SCOPE_UNIVERSE
        ifa_index: idx,
    };
    payload.extend_from_slice(unsafe { as_bytes(&ifa) });

    let octets = addr.octets();
    push_attr(&mut payload, libc::IFA_LOCAL, &octets);
    push_attr(&mut payload, libc::IFA_ADDRESS, &octets);

    let total_len = std::mem::size_of::<libc::nlmsghdr>() + payload.len();
    let hdr = libc::nlmsghdr {
        nlmsg_len: total_len as u32,
        nlmsg_type: libc::RTM_NEWADDR,
        nlmsg_flags: NLM_F_REQUEST | NLM_F_CREATE | NLM_F_EXCL | NLM_F_ACK,
        nlmsg_seq: 3,
        nlmsg_pid: 0,
    };

    let mut msg = Vec::with_capacity(total_len);
    msg.extend_from_slice(unsafe { as_bytes(&hdr) });
    msg.extend_from_slice(&payload);

    nl_exec(&sock, &msg).map_err(JailError::Network)
}

/// Bring an interface up via ioctl (same pattern as setup_loopback).
pub fn set_link_up(ifname: &str) -> Result<()> {
    validate_ifname(ifname).map_err(JailError::Network)?;

    // SAFETY: Creating a UDP socket for ioctl only. CLOEXEC prevents leak to children.
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if sock < 0 {
        return Err(JailError::Network(std::io::Error::last_os_error()));
    }

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    write_ifr_name(&mut ifr, ifname);
    ifr.ifr_ifru.ifru_flags = libc::IFF_UP as i16;

    // SAFETY: Valid socket and properly initialized ifreq.
    let ret = unsafe { libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &ifr) };
    let err = std::io::Error::last_os_error();
    unsafe { libc::close(sock) };

    if ret < 0 {
        return Err(JailError::Network(err));
    }
    Ok(())
}

/// Add a default route via a gateway.
pub fn add_default_route(gateway: Ipv4Addr) -> Result<()> {
    let sock = nl_socket().map_err(JailError::Network)?;

    let mut payload = Vec::new();
    let rtm = Rtmsg {
        rtm_family: libc::AF_INET as u8,
        rtm_dst_len: 0, // default route
        rtm_src_len: 0,
        rtm_tos: 0,
        rtm_table: libc::RT_TABLE_MAIN,
        rtm_protocol: libc::RTPROT_BOOT,
        rtm_scope: libc::RT_SCOPE_UNIVERSE,
        rtm_type: libc::RTN_UNICAST,
        rtm_flags: 0,
    };
    payload.extend_from_slice(unsafe { as_bytes(&rtm) });
    push_attr(&mut payload, libc::RTA_GATEWAY, &gateway.octets());

    let total_len = std::mem::size_of::<libc::nlmsghdr>() + payload.len();
    let hdr = libc::nlmsghdr {
        nlmsg_len: total_len as u32,
        nlmsg_type: libc::RTM_NEWROUTE,
        nlmsg_flags: NLM_F_REQUEST | NLM_F_CREATE | NLM_F_EXCL | NLM_F_ACK,
        nlmsg_seq: 4,
        nlmsg_pid: 0,
    };

    let mut msg = Vec::with_capacity(total_len);
    msg.extend_from_slice(unsafe { as_bytes(&hdr) });
    msg.extend_from_slice(&payload);

    nl_exec(&sock, &msg).map_err(JailError::Network)
}

/// Delete a network interface by name.
pub fn delete_link(ifname: &str) -> Result<()> {
    let idx = match ifindex(ifname) {
        Ok(i) => i,
        Err(_) => return Ok(()), // already gone
    };
    let sock = nl_socket().map_err(JailError::Network)?;

    let mut payload = Vec::new();
    let mut ifinfo: libc::ifinfomsg = unsafe { std::mem::zeroed() };
    ifinfo.ifi_index = idx as i32;
    payload.extend_from_slice(unsafe { as_bytes(&ifinfo) });

    let total_len = std::mem::size_of::<libc::nlmsghdr>() + payload.len();
    let hdr = libc::nlmsghdr {
        nlmsg_len: total_len as u32,
        nlmsg_type: libc::RTM_DELLINK,
        nlmsg_flags: NLM_F_REQUEST | NLM_F_ACK,
        nlmsg_seq: 5,
        nlmsg_pid: 0,
    };

    let mut msg = Vec::with_capacity(total_len);
    msg.extend_from_slice(unsafe { as_bytes(&hdr) });
    msg.extend_from_slice(&payload);

    nl_exec(&sock, &msg).map_err(JailError::Network)
}
