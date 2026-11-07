//! Outbound-address policy for operator-supplied URLs (workspace
//! `backend_url`s forwarded by the gateway).
//!
//! A `backend_url` is attacker-controlled input from any tenant with an
//! API key, and the gateway fetches it from inside the control-plane
//! network. Without a policy that is a textbook SSRF: point a domain at
//! `http://169.254.169.254/` (cloud metadata), `http://127.0.0.1:7000/`
//! (the control plane itself) or `http://postgres:5432/`.
//!
//! The rule is simple: only globally routable unicast addresses are
//! allowed. The check runs twice — once when the workspace is created
//! (fail fast with a 400) and again at connect time in the gateway's DNS
//! resolver, which hands the HTTP client *only* the vetted addresses so a
//! DNS-rebinding flip between the two checks cannot win.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::error::{CtlError, Result};

/// `true` when `ip` must never be reached from a tenant-supplied URL:
/// loopback, private, link-local (incl. `169.254.169.254` metadata),
/// CGNAT, multicast, documentation / benchmarking / reserved ranges, and
/// the IPv6 equivalents (ULA, link-local, mapped/compat/NAT64/6to4
/// wrappers around a forbidden IPv4).
#[must_use]
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => is_forbidden_v6(v6),
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || o[0] == 0                                    // 0.0.0.0/8 "this network"
        || (o[0] == 100 && (o[1] & 0xc0) == 64)         // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)      // 192.0.0.0/24 IETF
        || (o[0] == 192 && o[1] == 88 && o[2] == 99)    // 192.88.99.0/24 6to4 relay
        || (o[0] == 198 && (o[1] & 0xfe) == 18)         // 198.18.0.0/15 benchmarking
        || o[0] >= 240 // 240.0.0.0/4 reserved
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return true;
    }
    let s = ip.segments();
    // IPv4-mapped ::ffff:a.b.c.d and IPv4-compatible ::a.b.c.d.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_forbidden_v4(v4);
    }
    if s[0..6] == [0, 0, 0, 0, 0, 0] {
        return is_forbidden_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return is_forbidden_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    // 6to4 2002:AABB:CCDD::/48 embeds an IPv4.
    if s[0] == 0x2002 {
        return is_forbidden_v4(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    (s[0] & 0xfe00) == 0xfc00            // fc00::/7 unique local (incl. fd00:ec2::254)
        || (s[0] & 0xffc0) == 0xfe80     // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0     // fec0::/10 site-local (deprecated)
        || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1) // 64:ff9b:1::/48 local NAT64
        || (s[0] == 0x0100 && s[1..4] == [0, 0, 0])      // 100::/64 discard
        || (s[0] == 0x2001 && s[1] < 0x0200)             // 2001::/23 IETF (Teredo, ORCHID…)
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
}

/// A syntactically valid backend URL, split into the parts the policy
/// and the gateway need.
#[derive(Debug, Clone)]
pub struct BackendTarget {
    /// The original URL, normalised by the `url` crate.
    pub url: url::Url,
    /// Host as written (DNS name or IP literal, without brackets).
    pub host: String,
    /// Explicit or scheme-default port.
    pub port: u16,
    /// `Some` when the host is an IP literal.
    pub ip: Option<IpAddr>,
}

/// Parse + syntactically validate a `backend_url`: `http`/`https` only,
/// a host is required, embedded credentials are rejected.
pub fn parse_backend_url(raw: &str) -> Result<BackendTarget> {
    let bad = |why: &str| CtlError::BadRequest(format!("backend_url {why} (got {raw:?})"));
    if raw.len() > 2048 {
        return Err(bad("is too long"));
    }
    let url = url::Url::parse(raw).map_err(|_| bad("is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad("must use http:// or https://"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("must not embed credentials"));
    }
    let (host, ip) = match url.host() {
        Some(url::Host::Domain(d)) => (d.to_ascii_lowercase(), None),
        Some(url::Host::Ipv4(v4)) => (v4.to_string(), Some(IpAddr::V4(v4))),
        Some(url::Host::Ipv6(v6)) => (v6.to_string(), Some(IpAddr::V6(v6))),
        None => return Err(bad("must include a host")),
    };
    let port = url
        .port_or_known_default()
        .ok_or_else(|| bad("has no port"))?;
    Ok(BackendTarget {
        url,
        host,
        port,
        ip,
    })
}

/// Full create-time check: syntax, then resolve the host and require
/// every address to be publicly routable. `allow_private` skips the
/// address policy (dev / tests talking to a loopback upstream).
pub async fn check_backend_url(raw: &str, allow_private: bool) -> Result<BackendTarget> {
    let t = parse_backend_url(raw)?;
    if allow_private {
        return Ok(t);
    }
    let addrs: Vec<SocketAddr> = match t.ip {
        Some(ip) => vec![SocketAddr::new(ip, t.port)],
        None => tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::lookup_host((t.host.as_str(), t.port)),
        )
        .await
        .map_err(|_| {
            CtlError::BadRequest(format!(
                "backend_url host {:?} did not resolve in time",
                t.host
            ))
        })?
        .map_err(|_| {
            CtlError::BadRequest(format!("backend_url host {:?} does not resolve", t.host))
        })?
        .collect(),
    };
    if addrs.is_empty() {
        return Err(CtlError::BadRequest(format!(
            "backend_url host {:?} does not resolve",
            t.host
        )));
    }
    if let Some(a) = addrs.iter().find(|a| is_forbidden_ip(a.ip())) {
        return Err(CtlError::BadRequest(format!(
            "backend_url resolves to a non-public address ({}); loopback, private, \
             link-local, metadata and reserved ranges are not allowed",
            a.ip()
        )));
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn forbidden_v4_ranges() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "0.1.2.3",
            "100.64.0.1",
            "100.127.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.19.255.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "192.88.99.1",
        ] {
            assert!(is_forbidden_ip(ip(s)), "{s} should be forbidden");
        }
    }

    #[test]
    fn public_v4_allowed() {
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "172.32.0.1",
            "100.128.0.1",
        ] {
            assert!(!is_forbidden_ip(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn forbidden_v6_ranges() {
        for s in [
            "::",
            "::1",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
            "2001:db8::1",
            "2001::1",
            "100::1",
        ] {
            assert!(is_forbidden_ip(ip(s)), "{s} should be forbidden");
        }
    }

    #[test]
    fn public_v6_allowed() {
        for s in [
            "2606:4700:4700::1111",
            "2a00:1450:4001::200e",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_forbidden_ip(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn parse_rejects_bad_schemes_and_userinfo() {
        assert!(parse_backend_url("ftp://example.com/").is_err());
        assert!(parse_backend_url("file:///etc/passwd").is_err());
        assert!(parse_backend_url("gopher://x/").is_err());
        assert!(parse_backend_url("http://user:pw@example.com/").is_err());
        assert!(parse_backend_url("http://").is_err());
        assert!(parse_backend_url("not a url").is_err());
        let t = parse_backend_url("https://Example.COM/x").unwrap();
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, 443);
        let t = parse_backend_url("http://[::1]:8080/").unwrap();
        assert_eq!(t.ip, Some(ip("::1")));
    }

    #[tokio::test]
    async fn check_rejects_literal_private_and_metadata() {
        for u in [
            "http://127.0.0.1:7000/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://[::ffff:10.0.0.1]/",
            "http://0x7f000001/", // url crate normalises to 127.0.0.1
            "http://2130706433/", // decimal form of 127.0.0.1
        ] {
            assert!(check_backend_url(u, false).await.is_err(), "{u} accepted");
        }
    }

    #[tokio::test]
    async fn check_rejects_names_resolving_to_loopback() {
        assert!(
            check_backend_url("http://localhost:7000/", false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn allow_private_bypasses_address_policy_only() {
        assert!(check_backend_url("http://127.0.0.1:1/", true).await.is_ok());
        assert!(check_backend_url("ftp://127.0.0.1/", true).await.is_err());
    }
}
