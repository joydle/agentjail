//! Lightweight HTTP CONNECT proxy for network allowlisting.
//!
//! Runs in the parent process with real network access. Jailed processes
//! reach it via a veth pair. DNS is resolved at connection time.
//!
//! Everything a jail can make the proxy do is bounded, because the proxy
//! runs in the host process, outside the jail's cgroup:
//!
//! - **who** may talk to it: only the jail's own veth address
//!   ([`ProxyConfig::allowed_peer`]);
//! - **how many** tunnels: [`ProxyConfig::max_connections`] (default 64);
//! - **where** to: allowlisted host names, on allowlisted ports
//!   ([`ProxyConfig::allowed_ports`], default `[443]`), resolving only to
//!   public unicast addresses ([`is_public_routable`]);
//! - **how long**: the request head must arrive within
//!   [`ProxyConfig::request_timeout`], the upstream connect must finish
//!   within [`ProxyConfig::connect_timeout`] (10 s), and a tunnel with no
//!   traffic in either direction for [`ProxyConfig::idle_timeout`]
//!   (5 min) is closed;
//! - **how much** is buffered: each request/header line is capped at
//!   [`MAX_LINE_BYTES`], the head at [`MAX_HEADER_LINES`] lines and
//!   [`MAX_HEADER_BYTES`] total.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

/// Maximum bytes for a single request line or header line.
const MAX_LINE_BYTES: usize = 8192;
/// Maximum number of header lines after the request line.
const MAX_HEADER_LINES: usize = 64;
/// Maximum total bytes of the request head (request line + headers).
const MAX_HEADER_BYTES: usize = 32 * 1024;

/// Default per-jail cap on concurrent proxy connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;
/// Default upstream connect timeout (DNS resolution + TCP handshake).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default tunnel idle timeout.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Default deadline for receiving the full CONNECT request head.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Default destination-port allowlist: HTTPS only.
pub const DEFAULT_ALLOWED_PORTS: &[u16] = &[443];

/// Env var overriding [`ProxyConfig::allowed_ports`] for proxies spawned
/// by the jail runtime: comma-separated port numbers, e.g. `443,8443`.
pub const ALLOWED_PORTS_ENV: &str = "AGENTJAIL_PROXY_ALLOWED_PORTS";

/// A parsed domain allowlist pattern. Pre-parsed at construction time
/// so `is_allowed` doesn't re-parse on every connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainPattern {
    /// Exact match (case-insensitive): "api.anthropic.com"
    Exact(String),
    /// Wildcard: "*.example.com" matches any subdomain and the base domain.
    /// Stores the lowercased base (e.g., "example.com") and suffix (".example.com").
    Wildcard { base: String, suffix: String },
}

impl DomainPattern {
    /// Parse a domain pattern string.
    pub fn parse(pattern: &str) -> Self {
        let lower = pattern.to_lowercase();
        if let Some(base) = lower.strip_prefix("*.") {
            DomainPattern::Wildcard {
                suffix: format!(".{base}"),
                base: base.to_string(),
            }
        } else {
            DomainPattern::Exact(lower)
        }
    }

    /// Check if a host matches this pattern.
    pub fn matches(&self, host: &str) -> bool {
        let host_lower = host.to_lowercase();
        match self {
            DomainPattern::Exact(domain) => host_lower == *domain,
            DomainPattern::Wildcard { base, suffix } => {
                host_lower.ends_with(suffix.as_str()) || host_lower == *base
            }
        }
    }
}

/// Proxy configuration.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Allowed domain patterns (pre-parsed).
    pub allowlist: Vec<DomainPattern>,
    /// Port to listen on (default: 8080).
    pub port: u16,
    /// IP address to bind to (default: localhost).
    pub bind_ip: IpAddr,
    /// If set, connections whose source address isn't exactly this IP are
    /// dropped on accept. The jail runtime sets it to the jail's own veth
    /// address so a neighbouring jail can't borrow this jail's allowlist.
    pub allowed_peer: Option<IpAddr>,
    /// Destination ports a CONNECT may target (default: `[443]`).
    pub allowed_ports: Vec<u16>,
    /// Concurrent connection cap; excess connections get `503` and are
    /// closed (default: 64).
    pub max_connections: usize,
    /// Upstream DNS + TCP connect timeout (default: 10 s).
    pub connect_timeout: Duration,
    /// Tunnel idle timeout (default: 5 min).
    pub idle_timeout: Duration,
    /// Deadline for the client to send its full request head (default: 10 s).
    pub request_timeout: Duration,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            allowlist: Vec::new(),
            port: 8080,
            bind_ip: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            allowed_peer: None,
            allowed_ports: DEFAULT_ALLOWED_PORTS.to_vec(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

/// Parse a comma-separated port list (the [`ALLOWED_PORTS_ENV`] format).
/// Returns `None` if any element is not a valid non-zero port.
pub fn parse_port_list(s: &str) -> Option<Vec<u16>> {
    let ports: Option<Vec<u16>> = s
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u16>().ok().filter(|&n| n != 0))
        .collect();
    ports.filter(|v| !v.is_empty())
}

/// Per-connection policy, shared by every handler task.
#[derive(Debug)]
struct ConnPolicy {
    allowlist: Vec<DomainPattern>,
    allowed_ports: Vec<u16>,
    connect_timeout: Duration,
    idle_timeout: Duration,
    request_timeout: Duration,
}

impl From<&ProxyConfig> for ConnPolicy {
    fn from(c: &ProxyConfig) -> Self {
        Self {
            allowlist: c.allowlist.clone(),
            allowed_ports: c.allowed_ports.clone(),
            connect_timeout: c.connect_timeout,
            idle_timeout: c.idle_timeout,
            request_timeout: c.request_timeout,
        }
    }
}

/// Run the proxy server.
///
/// Sends `Ok(())` on `ready` once bound, or `Err(msg)` on bind failure.
/// Stops when `shutdown` is dropped or receives a signal.
pub async fn run_proxy(
    config: ProxyConfig,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()> {
    let addr = SocketAddr::new(config.bind_ip, config.port);
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => {
            let _ = ready.send(Ok(()));
            l
        }
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return Err(e);
        }
    };
    serve(listener, &config, shutdown).await
}

/// Accept loop on an already-bound listener.
async fn serve(
    listener: TcpListener,
    config: &ProxyConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()> {
    let policy = Arc::new(ConnPolicy::from(config));
    let permits = Arc::new(Semaphore::new(config.max_connections.max(1)));
    let allowed_peer = config.allowed_peer;

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer) = match result {
                    Ok(x) => x,
                    // Transient accept errors (EMFILE, ECONNABORTED) must
                    // not kill the jail's only route out.
                    Err(e) => {
                        eprintln!("proxy accept error: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                if let Some(want) = allowed_peer
                    && !same_ip(peer.ip(), want)
                {
                    // Not our jail: drop without a word.
                    drop(stream);
                    continue;
                }
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    // Best-effort, non-blocking refusal; never spawns, so
                    // a connection flood can't grow tasks past the cap.
                    // (tokio's `try_write` would see no readiness yet on a
                    // fresh socket; the std socket is non-blocking and its
                    // send buffer is empty, so the write just succeeds.)
                    if let Ok(std_stream) = stream.into_std() {
                        use std::io::Write as _;
                        let _ = (&std_stream).write(b"HTTP/1.1 503 Service Unavailable\r\n\r\n");
                    }
                    continue;
                };
                let policy = policy.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_connection(stream, &policy).await {
                        eprintln!("proxy error: {e}");
                    }
                });
            }
            _ = shutdown.changed() => {
                return Ok(());
            }
        }
    }
}

/// IP equality that treats `::ffff:a.b.c.d` and `a.b.c.d` as the same
/// address (a dual-stack listener reports IPv4 peers in mapped form).
fn same_ip(a: IpAddr, b: IpAddr) -> bool {
    a.to_canonical() == b.to_canonical()
}

/// Why the request head was rejected, mapped to an HTTP status.
enum HeadError {
    Io(std::io::Error),
    TooLarge,
}

/// Read and discard header lines up to the blank line, enforcing the
/// line-count and total-size caps. Returns the total bytes consumed.
async fn consume_headers<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    mut used: usize,
) -> Result<usize, HeadError> {
    for _ in 0..=MAX_HEADER_LINES {
        let mut line = String::new();
        let budget = MAX_LINE_BYTES.min(MAX_HEADER_BYTES.saturating_sub(used));
        read_line_bounded(reader, &mut line, budget)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    HeadError::TooLarge
                } else {
                    HeadError::Io(e)
                }
            })?;
        used += line.len();
        if line == "\r\n" || line == "\n" {
            return Ok(used);
        }
        if line.is_empty() {
            return Err(HeadError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed during request head",
            )));
        }
    }
    // One more line than allowed and still no blank line.
    Err(HeadError::TooLarge)
}

/// Handle a single client connection.
async fn handle_connection(mut client: TcpStream, policy: &ConnPolicy) -> std::io::Result<()> {
    // Phase 1: parse CONNECT request and validate against allowlist.
    let (target_stream, early_data) = {
        let (reader, mut writer) = client.split();
        let mut reader = BufReader::new(reader);

        // Bound the whole head, not just each line, so a slow-drip client
        // can't hold a connection slot forever.
        let head = tokio::time::timeout(policy.request_timeout, async {
            let mut request_line = String::with_capacity(256);
            read_line_bounded(&mut reader, &mut request_line, MAX_LINE_BYTES)
                .await
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::InvalidData {
                        HeadError::TooLarge
                    } else {
                        HeadError::Io(e)
                    }
                })?;
            let used = request_line.len();
            consume_headers(&mut reader, used).await?;
            Ok::<String, HeadError>(request_line)
        })
        .await;
        let request_line = match head {
            Ok(Ok(l)) => l,
            Ok(Err(HeadError::TooLarge)) => {
                writer
                    .write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n")
                    .await?;
                return Ok(());
            }
            Ok(Err(HeadError::Io(e))) => return Err(e),
            Err(_) => {
                writer
                    .write_all(b"HTTP/1.1 408 Request Timeout\r\n\r\n")
                    .await?;
                return Ok(());
            }
        };

        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 3 || parts[0] != "CONNECT" {
            writer
                .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                .await?;
            return Ok(());
        }

        let (host, port) = match parse_host_port(parts[1]) {
            Ok(hp) => hp,
            Err(_) => {
                writer
                    .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                    .await?;
                return Ok(());
            }
        };

        if !is_allowed(&host, &policy.allowlist) || !policy.allowed_ports.contains(&port) {
            writer.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
            return Ok(());
        }

        // Resolve the hostname ONCE and lock connections to the IPs
        // we approved here. Without this, `TcpStream::connect("host:port")`
        // runs its own resolution → attacker's DNS can swap the reply
        // between our allowlist check and the actual connect (classic
        // DNS-rebinding SSRF). Resolving ourselves + filtering private/
        // link-local/loopback ranges closes the rebinding and the
        // "allowlisted CDN that proxies metadata endpoints" pivot.
        let resolved: Vec<SocketAddr> = match tokio::time::timeout(
            policy.connect_timeout,
            tokio::net::lookup_host(format!("{host}:{port}")),
        )
        .await
        {
            Ok(Ok(it)) => it.collect(),
            _ => Vec::new(),
        };
        let routable: Vec<SocketAddr> = resolved
            .into_iter()
            .filter(|sa| is_public_routable(&sa.ip()))
            .collect();
        if routable.is_empty() {
            writer.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
            return Ok(());
        }

        // Connect to the pre-resolved, pre-filtered IP. Try each
        // address until one succeeds (cheap happy-eyeballs).
        let stream = match connect_any(&routable, policy.connect_timeout).await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("HTTP/1.1 502 Bad Gateway\r\n\r\n{e}");
                writer.write_all(msg.as_bytes()).await?;
                return Ok(());
            }
        };

        writer
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        // Bytes the client pipelined after the head (e.g. a TLS
        // ClientHello sent optimistically) are already in our buffer;
        // forward them instead of silently dropping them.
        (stream, reader.buffer().to_vec())
    }; // reader + writer dropped here, releasing borrow on client

    // Phase 2: bidirectional tunnel.
    let mut target_stream = target_stream;
    if !early_data.is_empty() {
        target_stream.write_all(&early_data).await?;
    }
    tunnel(client, target_stream, policy.idle_timeout).await
}

/// Connect to the first address that accepts, giving the whole attempt
/// (all addresses) at most `timeout`.
async fn connect_any(addrs: &[SocketAddr], timeout: Duration) -> std::io::Result<TcpStream> {
    let attempt = async {
        let mut last_err: Option<std::io::Error> = None;
        for sa in addrs {
            match TcpStream::connect(sa).await {
                Ok(s) => return Ok(s),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| std::io::Error::other("no addresses")))
    };
    match tokio::time::timeout(timeout, attempt).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("upstream connect timed out after {timeout:?}"),
        )),
    }
}

/// Shuttle bytes both ways until both sides have closed, an error
/// occurs, or nothing moves in either direction for `idle`.
///
/// Half-closes are propagated (EOF from one side → `shutdown()` of the
/// other's write half) so request/response protocols that close their
/// sending side still get their answer.
async fn tunnel<A, B>(a: A, b: B, idle: Duration) -> std::io::Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut ar, mut aw) = tokio::io::split(a);
    let (mut br, mut bw) = tokio::io::split(b);
    let mut abuf = vec![0u8; 16 * 1024];
    let mut bbuf = vec![0u8; 16 * 1024];
    let (mut a_open, mut b_open) = (true, true);

    while a_open || b_open {
        tokio::select! {
            r = ar.read(&mut abuf), if a_open => {
                let n = r?;
                if n == 0 {
                    a_open = false;
                    let _ = bw.shutdown().await;
                } else {
                    timed(idle, bw.write_all(&abuf[..n])).await?;
                }
            }
            r = br.read(&mut bbuf), if b_open => {
                let n = r?;
                if n == 0 {
                    b_open = false;
                    let _ = aw.shutdown().await;
                } else {
                    timed(idle, aw.write_all(&bbuf[..n])).await?;
                }
            }
            _ = tokio::time::sleep(idle) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("tunnel idle for {idle:?}"),
                ));
            }
        }
    }
    Ok(())
}

/// A write that makes no progress for `idle` (peer stopped reading) is
/// as idle as a read that gets nothing.
async fn timed<F>(idle: Duration, f: F) -> std::io::Result<()>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    match tokio::time::timeout(idle, f).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "tunnel write stalled",
        )),
    }
}

/// Read a line from a buffered reader, rejecting lines over `max_bytes`.
async fn read_line_bounded<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut String,
    max_bytes: usize,
) -> std::io::Result<()> {
    // Bound the read itself: `read_line` alone buffers until a newline,
    // so a jail streaming bytes without one would grow `buf` without
    // limit in the host process (outside the jail's cgroup).
    let n = tokio::io::AsyncReadExt::take(&mut *reader, max_bytes as u64 + 1)
        .read_line(buf)
        .await?;
    if n > max_bytes {
        buf.clear();
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line too long",
        ));
    }
    Ok(())
}

/// Parse "host:port" string.
///
/// Accepts:
///   - `host:port`  — hostname or IPv4 literal
///   - `[ipv6]:port` — bracketed IPv6
///   - `host`       — defaults to port 443
///
/// Rejects unbracketed IPv6 (ambiguous with `host:port`): splitting at
/// the last `:` reads `::1:443` as host=`::1`, port=`443`, which with a
/// permissive allowlist makes the proxy an arbitrary-egress gadget.
/// IPv6 must use the RFC 3986 authority form.
fn parse_host_port(s: &str) -> std::io::Result<(String, u16)> {
    if let Some(rest) = s.strip_prefix('[') {
        // Bracketed IPv6 literal.
        let (v6, tail) = rest.split_once(']').ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unterminated IPv6 bracket",
            )
        })?;
        let port = if let Some(p) = tail.strip_prefix(':') {
            p.parse().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid port")
            })?
        } else if tail.is_empty() {
            443
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "garbage after IPv6 literal",
            ));
        };
        return Ok((v6.to_string(), port));
    }

    // Not bracketed: must be hostname or IPv4. Reject if the remainder
    // parses as an unbracketed IPv6 address (contains >= 2 colons in
    // the host portion) to avoid the ambiguity.
    match s.rsplit_once(':') {
        Some((host, port_str)) => {
            if host.contains(':') {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "IPv6 literal must be bracketed",
                ));
            }
            let port = port_str.parse().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid port")
            })?;
            Ok((host.to_string(), port))
        }
        None => Ok((s.to_string(), 443)),
    }
}

/// Check if host matches any pattern in allowlist.
fn is_allowed(host: &str, allowlist: &[DomainPattern]) -> bool {
    allowlist.iter().any(|p| p.matches(host))
}

/// Is `ip` safe to hand to the allowlist proxy as a destination?
///
/// Only globally routable unicast is allowed. Rejected:
///
/// IPv4: `0.0.0.0/8` ("this network" — `0.0.0.0` connects to the local
/// host on Linux), `10/8`, `100.64/10` (CGNAT), `127/8`, `169.254/16`
/// (link-local, cloud metadata), `172.16/12`, `192.0.0/24` (IETF
/// protocol assignments), `192.0.2/24`, `198.51.100/24`, `203.0.113/24`
/// (documentation), `192.88.99/24` (6to4 relay anycast), `192.168/16`,
/// `198.18/15` (benchmarking), `224/4` (multicast), `240/4` (reserved,
/// incl. broadcast).
///
/// IPv6: anything outside `2000::/3` (global unicast) — which covers
/// `::`, `::1`, `::/96` IPv4-compatible, `64:ff9b::/96` and
/// `64:ff9b:1::/48` (NAT64, which would let `64:ff9b::a9fe:a9fe` reach
/// the metadata endpoint), `100::/64` (discard), `fc00::/7` (ULA),
/// `fe80::/10` (link-local), `fec0::/10`, `ff00::/8` (multicast) — plus,
/// inside `2000::/3`: `2001::/23` (IETF special-purpose, incl. Teredo
/// `2001::/32` and ORCHID), `2001:db8::/32` and `3fff::/20`
/// (documentation), `2002::/16` (6to4, which embeds an arbitrary IPv4
/// address) and `5f00::/16` (SRv6 SIDs). IPv4-mapped (`::ffff:a.b.c.d`)
/// addresses are unwrapped and judged by the IPv4 rules.
///
/// Keeps the proxy from being a metadata-endpoint (169.254.169.254)
/// or private-network SSRF gadget when a jailed process's DNS points
/// an allowlisted name at such an address.
fn is_public_routable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(*v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let seg = v6.segments();
            // Global unicast only: 2000::/3.
            if (seg[0] & 0xe000) != 0x2000 {
                return false;
            }
            // 2001::/23 — IETF protocol assignments (Teredo 2001::/32,
            // benchmarking 2001:2::/48, ORCHID 2001:10::/28, …).
            if seg[0] == 0x2001 && seg[1] < 0x0200 {
                return false;
            }
            // 2001:db8::/32 — documentation.
            if seg[0] == 0x2001 && seg[1] == 0x0db8 {
                return false;
            }
            // 2002::/16 — 6to4 (embeds an arbitrary IPv4 address).
            if seg[0] == 0x2002 {
                return false;
            }
            // 3fff::/20 — documentation (RFC 9637).
            if (seg[0] & 0xfff0) == 0x3ff0 {
                return false;
            }
            true
        }
    }
}

fn is_public_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    let reject = a == 0                                  // 0.0.0.0/8
        || a == 10                                       // 10/8
        || (a == 100 && (64..=127).contains(&b))         // 100.64/10 CGNAT
        || a == 127                                      // loopback
        || (a == 169 && b == 254)                        // link-local
        || (a == 172 && (16..=31).contains(&b))          // 172.16/12
        || (a == 192 && b == 0 && c == 0)                // 192.0.0/24
        || (a == 192 && b == 0 && c == 2)                // TEST-NET-1
        || (a == 192 && b == 88 && c == 99)              // 6to4 relay
        || (a == 192 && b == 168)                        // 192.168/16
        || (a == 198 && (18..=19).contains(&b))          // benchmarking
        || (a == 198 && b == 51 && c == 100)             // TEST-NET-2
        || (a == 203 && b == 0 && c == 113)              // TEST-NET-3
        || a >= 224; // 224/4 multicast, 240/4 reserved, broadcast
    !reject
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_list(patterns: &[&str]) -> Vec<DomainPattern> {
        patterns.iter().map(|p| DomainPattern::parse(p)).collect()
    }

    #[test]
    fn test_is_allowed_empty_blocks_everything() {
        let allowlist = parse_list(&[]);
        assert!(!is_allowed("api.anthropic.com", &allowlist));
        assert!(!is_allowed("google.com", &allowlist));
        assert!(!is_allowed("localhost", &allowlist));
        assert!(!is_allowed("127.0.0.1", &allowlist));
    }

    #[test]
    fn test_is_allowed_exact() {
        let allowlist = parse_list(&["api.anthropic.com"]);
        assert!(is_allowed("api.anthropic.com", &allowlist));
        assert!(is_allowed("API.ANTHROPIC.COM", &allowlist));
        assert!(!is_allowed("evil.com", &allowlist));
        assert!(!is_allowed("anthropic.com", &allowlist));
        assert!(!is_allowed("sub.api.anthropic.com", &allowlist));
    }

    #[test]
    fn test_is_allowed_wildcard() {
        let allowlist = parse_list(&["*.openai.com"]);
        assert!(is_allowed("api.openai.com", &allowlist));
        assert!(is_allowed("chat.openai.com", &allowlist));
        assert!(is_allowed("openai.com", &allowlist));
        assert!(!is_allowed("openai.com.evil.com", &allowlist));
        assert!(!is_allowed("notopenai.com", &allowlist));
    }

    #[test]
    fn test_domain_pattern_parse() {
        assert_eq!(
            DomainPattern::parse("api.example.com"),
            DomainPattern::Exact("api.example.com".into())
        );
        assert!(matches!(
            DomainPattern::parse("*.example.com"),
            DomainPattern::Wildcard { .. }
        ));
    }

    #[test]
    fn test_parse_host_port() {
        let (h, p) = parse_host_port("example.com:443").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, 443);

        let (h, p) = parse_host_port("example.com:8080").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, 8080);

        let (h, p) = parse_host_port("example.com").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, 443);
    }

    #[tokio::test]
    async fn test_read_line_bounded_rejects_oversize() {
        let long_line = format!("{}\n", "A".repeat(10_000));
        let mut cursor = std::io::Cursor::new(long_line.into_bytes());
        let mut reader = tokio::io::BufReader::new(&mut cursor);
        let mut buf = String::new();

        let result = read_line_bounded(&mut reader, &mut buf, 8192).await;
        assert!(result.is_err(), "Should reject lines over MAX_LINE_BYTES");
    }

    #[tokio::test]
    async fn test_read_line_bounded_accepts_normal() {
        let line = "CONNECT example.com:443 HTTP/1.1\r\n";
        let mut cursor = std::io::Cursor::new(line.as_bytes().to_vec());
        let mut reader = tokio::io::BufReader::new(&mut cursor);
        let mut buf = String::new();

        let result = read_line_bounded(&mut reader, &mut buf, 8192).await;
        assert!(result.is_ok(), "Should accept normal-sized lines");
        assert!(buf.contains("CONNECT"));
    }

    /// Helper: send a CONNECT request through the proxy and return the response status line.
    async fn proxy_connect(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .expect("connect to proxy");
        let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .await
            .expect("read response");
        response
    }

    #[tokio::test]
    async fn test_proxy_blocks_disallowed_domain() {
        // Bind to a random port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let allowlist = Arc::new(parse_list(&["allowed.example.com"]));

        // Spawn proxy handler for one connection
        let al = allowlist.clone();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, &pol((*al).clone()))
                .await
                .unwrap();
        });

        let response = proxy_connect(port, "evil.com:443").await;
        assert!(
            response.contains("403"),
            "Disallowed domain should get 403, got: {}",
            response.trim()
        );
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_proxy_blocks_everything_when_empty_allowlist() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let allowlist: Arc<Vec<DomainPattern>> = Arc::new(vec![]);

        let al = allowlist.clone();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, &pol((*al).clone()))
                .await
                .unwrap();
        });

        let response = proxy_connect(port, "google.com:443").await;
        assert!(
            response.contains("403"),
            "Empty allowlist should block everything, got: {}",
            response.trim()
        );
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_proxy_rejects_non_connect_method() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let allowlist = Arc::new(parse_list(&["anything.com"]));

        let al = allowlist.clone();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, &pol((*al).clone()))
                .await
                .unwrap();
        });

        // Send a GET instead of CONNECT
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        stream
            .write_all(b"GET http://anything.com/ HTTP/1.1\r\nHost: anything.com\r\n\r\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader.read_line(&mut response).await.unwrap();
        assert!(
            response.contains("400"),
            "Non-CONNECT should get 400, got: {}",
            response.trim()
        );
        let _ = handle.await;
    }

    fn pol(allowlist: Vec<DomainPattern>) -> ConnPolicy {
        ConnPolicy::from(&ProxyConfig {
            allowlist,
            ..ProxyConfig::default()
        })
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn public_routable_rejects_special_v4() {
        for a in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "127.255.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!is_public_routable(&ip(a)), "{a} must be rejected");
        }
        for a in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "172.32.0.1",
            "198.20.0.1",
            "223.255.255.254",
        ] {
            assert!(is_public_routable(&ip(a)), "{a} must be allowed");
        }
    }

    #[test]
    fn public_routable_rejects_special_v6() {
        for a in [
            "::",
            "::1",
            // ::/96 IPv4-compatible (deprecated), incl. embedded private.
            "::a9fe:a9fe",
            "::10.0.0.1",
            "::8.8.8.8",
            // IPv4-mapped private / loopback / metadata / this-network.
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "::ffff:169.254.169.254",
            "::ffff:0.0.0.0",
            "::ffff:192.168.0.1",
            // NAT64 well-known + local-use prefixes.
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::808:808",
            "64:ff9b:1::1",
            // Discard-only.
            "100::1",
            // Teredo + the rest of 2001::/23.
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:2::1",
            "2001:10::1",
            "2001:1ff::1",
            // Documentation.
            "2001:db8::1",
            "3fff::1",
            "3fff:fff::1",
            // 6to4.
            "2002:a9fe:a9fe::1",
            "2002:0808:0808::1",
            // ULA, link-local, site-local, multicast.
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "ff0e::1",
            // SRv6 SIDs.
            "5f00::1",
        ] {
            assert!(!is_public_routable(&ip(a)), "{a} must be rejected");
        }
        for a in [
            "2606:4700:4700::1111",
            "2a00:1450:4001::200e",
            "2001:200::1",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public_routable(&ip(a)), "{a} must be allowed");
        }
    }

    #[test]
    fn port_list_parsing() {
        assert_eq!(parse_port_list("443"), Some(vec![443]));
        assert_eq!(parse_port_list(" 443, 8443 ,"), Some(vec![443, 8443]));
        assert_eq!(parse_port_list(""), None);
        assert_eq!(parse_port_list("0"), None);
        assert_eq!(parse_port_list("443,abc"), None);
        assert_eq!(parse_port_list("70000"), None);
    }

    /// Spawn a full accept loop (`serve`) for `cfg` on an ephemeral
    /// loopback port.
    async fn spawn_serve(mut cfg: ProxyConfig) -> (u16, tokio::sync::watch::Sender<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        cfg.port = port;
        let (tx, rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let _ = serve(listener, &cfg, rx).await;
        });
        (port, tx)
    }

    async fn read_status(stream: TcpStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;
        line
    }

    #[tokio::test]
    async fn non_allowlisted_port_is_forbidden() {
        // An allowlisted *public IP literal* resolves without DNS and
        // passes the routability filter, so only the port allowlist can
        // stop it (without it the proxy dials 1.1.1.1:22). The port
        // check runs before any connect, so this needs no network.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, &pol(parse_list(&["1.1.1.1"])))
                .await
                .unwrap();
        });
        let t0 = std::time::Instant::now();
        let response = proxy_connect(port, "1.1.1.1:22").await;
        assert!(response.contains("403"), "got: {response}");
        assert!(t0.elapsed() < Duration::from_secs(2));
        let _ = handle.await;
    }

    #[tokio::test]
    async fn too_many_headers_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = handle_connection(stream, &pol(parse_list(&["a.example"]))).await;
        });
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut req = String::from("CONNECT a.example:443 HTTP/1.1\r\n");
        for i in 0..(MAX_HEADER_LINES + 5) {
            req.push_str(&format!("X-H{i}: v\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let status = read_status(s).await;
        assert!(status.contains("431"), "got: {status:?}");
        let _ = handle.await;
    }

    #[tokio::test]
    async fn oversized_header_block_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = handle_connection(stream, &pol(parse_list(&["a.example"]))).await;
        });
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // Each line is under MAX_LINE_BYTES, but together they exceed
        // MAX_HEADER_BYTES while staying under the line-count cap.
        let mut req = String::from("CONNECT a.example:443 HTTP/1.1\r\n");
        let val = "v".repeat(MAX_LINE_BYTES - 100);
        for i in 0..((MAX_HEADER_BYTES / val.len()) + 2) {
            req.push_str(&format!("X-H{i}: {val}\r\n"));
        }
        req.push_str("\r\n");
        let _ = s.write_all(req.as_bytes()).await;
        let status = read_status(s).await;
        assert!(status.contains("431"), "got: {status:?}");
        let _ = handle.await;
    }

    #[tokio::test]
    async fn slow_request_head_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut p = pol(parse_list(&["a.example"]));
        p.request_timeout = Duration::from_millis(200);
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = handle_connection(stream, &p).await;
        });
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(b"CONNECT a.example:443 HTTP/1.1\r\n")
            .await
            .unwrap();
        // ...and never send the blank line.
        let t0 = std::time::Instant::now();
        let status = read_status(s).await;
        assert!(status.contains("408"), "got: {status:?}");
        assert!(t0.elapsed() < Duration::from_secs(3));
        let _ = handle.await;
    }

    #[tokio::test]
    async fn connection_cap_enforced() {
        let (port, _stop) = spawn_serve(ProxyConfig {
            allowlist: parse_list(&["a.example"]),
            max_connections: 2,
            ..ProxyConfig::default()
        })
        .await;
        // Two idle connections hold both permits.
        let c1 = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let c2 = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let c3 = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let status = read_status(c3).await;
        assert!(
            status.contains("503"),
            "third connection must be refused, got {status:?}"
        );

        // Free a slot: a new connection is served again.
        drop(c1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut c4 = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        c4.write_all(b"CONNECT evil.example:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let status = read_status(c4).await;
        assert!(status.contains("403"), "got {status:?}");
        drop(c2);
    }

    /// Source-address pinning: only the configured peer may use the
    /// proxy (Linux: the whole 127/8 is local, so we can pick sources).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn rejects_peer_that_is_not_its_jail() {
        let (port, _stop) = spawn_serve(ProxyConfig {
            allowlist: parse_list(&["a.example"]),
            allowed_peer: Some(ip("127.0.0.2")),
            ..ProxyConfig::default()
        })
        .await;

        let connect_from = |src: &'static str| async move {
            let sock = tokio::net::TcpSocket::new_v4().unwrap();
            sock.bind(format!("{src}:0").parse().unwrap()).unwrap();
            let mut s = sock
                .connect(format!("127.0.0.1:{port}").parse().unwrap())
                .await
                .unwrap();
            let _ = s
                .write_all(b"CONNECT evil.example:443 HTTP/1.1\r\n\r\n")
                .await;
            read_status(s).await
        };

        let foreign = connect_from("127.0.0.3").await;
        assert!(
            foreign.is_empty(),
            "foreign peer must get no response, got {foreign:?}"
        );
        let own = connect_from("127.0.0.2").await;
        assert!(own.contains("403"), "own peer must be served, got {own:?}");
    }

    #[tokio::test]
    async fn connect_timeout_is_bounded() {
        // A non-routable target: either the stack fails fast or our
        // timeout fires — never the OS default of minutes.
        let t0 = std::time::Instant::now();
        let r = connect_any(
            &["10.255.255.1:443".parse().unwrap()],
            Duration::from_millis(300),
        )
        .await;
        assert!(r.is_err());
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "took {:?}",
            t0.elapsed()
        );
    }

    #[tokio::test]
    async fn tunnel_closes_when_idle() {
        let (a, _a_peer) = tokio::io::duplex(1024);
        let (b, _b_peer) = tokio::io::duplex(1024);
        let t0 = std::time::Instant::now();
        let r = tunnel(a, b, Duration::from_millis(150)).await;
        assert!(r.is_err(), "idle tunnel must end with a timeout");
        assert!(t0.elapsed() >= Duration::from_millis(140));
        assert!(t0.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn tunnel_forwards_and_propagates_half_close() {
        let (a, mut a_peer) = tokio::io::duplex(1024);
        let (b, mut b_peer) = tokio::io::duplex(1024);
        let t = tokio::spawn(tunnel(a, b, Duration::from_secs(5)));
        a_peer.write_all(b"ping").await.unwrap();
        a_peer.shutdown().await.unwrap();
        let mut got = Vec::new();
        b_peer.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"ping");
        b_peer.write_all(b"pong").await.unwrap();
        b_peer.shutdown().await.unwrap();
        let mut back = Vec::new();
        a_peer.read_to_end(&mut back).await.unwrap();
        assert_eq!(back, b"pong");
        t.await.unwrap().unwrap();
    }
}
