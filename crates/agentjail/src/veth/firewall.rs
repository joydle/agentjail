//! Host packet filter that isolates jails from each other and from the
//! host.
//!
//! Without it, a jail's default route points at its host-side veth
//! address, so it can open TCP connections to *any* address the host
//! owns — including another jail's proxy (`10.B.B.1:8080`) and every
//! service the host bound to `0.0.0.0` — and, if the host forwards,
//! route into other jails' subnets.
//!
//! We install one static nftables table, `inet agentjail`, that matches
//! every `aj-h*` interface by name, so nothing per-jail has to be added
//! or removed:
//!
//! ```text
//! table inet agentjail {
//!   chain input {   # packets from a jail to the host itself
//!     type filter hook input priority -10; policy accept;
//!     iifname "aj-h*" ct state established,related accept   # replies to host-initiated flows
//!     iifname "aj-h*" meta nfproto ipv4 tcp dport 8080 fib daddr . iif type local accept
//!     iifname "aj-h*" drop
//!   }
//!   chain forward { # anything routed through the host
//!     type filter hook forward priority -10; policy accept;
//!     iifname "aj-h*" drop
//!     oifname "aj-h*" drop
//!   }
//! }
//! ```
//!
//! `fib daddr . iif type local` is true only when the destination is an
//! address configured on the interface the packet arrived on — i.e. the
//! jail's *own* gateway address. So a jail can reach its own proxy and
//! nothing else on the host; host-initiated connections into a jail
//! (gateway port-forwarding) keep working through the `established`
//! rule. Policies stay `accept`, so non-jail traffic is untouched, and a
//! `drop` here is final regardless of other tables (docker, firewalld).
//!
//! The table is (re)installed atomically with `nft -f -`; remove it with
//! `nft delete table inet agentjail`.
//!
//! # Configuration: `JailConfig::firewall` / `AGENTJAIL_FIREWALL`
//!
//! [`JailConfig::firewall`](crate::JailConfig::firewall) picks the mode;
//! when it is `None` the `AGENTJAIL_FIREWALL` env var does:
//!
//! - `auto` (default): install if `nft` is available; otherwise warn once
//!   and rely on the proxy's source-address check, which protects the
//!   proxies but **not** other host listeners.
//! - `nft` / `require`: installation failure is an error.
//! - `off`: don't touch the host firewall (manage it yourself).
//!
//! An iptables backend is not provided; on iptables-legacy-only hosts use
//! `off` and install equivalent rules out of band.

use crate::error::{JailError, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Environment variable selecting the firewall mode.
pub(crate) const ENV: &str = "AGENTJAIL_FIREWALL";
/// nftables table name (family `inet`).
pub(crate) const TABLE: &str = "agentjail";

// How hard to try to install the host firewall.
pub(crate) use crate::config::FirewallMode as Mode;

/// Parse an `AGENTJAIL_FIREWALL` value.
pub(crate) fn parse_mode(s: &str) -> Option<Mode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Some(Mode::Auto),
        "nft" | "require" | "required" | "on" | "1" => Some(Mode::Require),
        "off" | "none" | "0" | "disabled" => Some(Mode::Off),
        _ => None,
    }
}

fn mode_from_env() -> Result<Mode> {
    match std::env::var(ENV) {
        Err(_) => Ok(Mode::Auto),
        Ok(v) => parse_mode(&v).ok_or_else(|| {
            JailError::Network(std::io::Error::other(format!(
                "invalid {ENV}={v:?} (expected auto, nft or off)"
            )))
        }),
    }
}

/// The nft script that atomically replaces the `inet agentjail` table.
pub(crate) fn ruleset(proxy_port: u16) -> String {
    format!(
        "add table inet {TABLE}\n\
         delete table inet {TABLE}\n\
         table inet {TABLE} {{\n\
         \tchain input {{\n\
         \t\ttype filter hook input priority -10; policy accept;\n\
         \t\tiifname \"aj-h*\" ct state established,related accept\n\
         \t\tiifname \"aj-h*\" meta nfproto ipv4 tcp dport {proxy_port} fib daddr . iif type local accept\n\
         \t\tiifname \"aj-h*\" counter drop\n\
         \t}}\n\
         \tchain forward {{\n\
         \t\ttype filter hook forward priority -10; policy accept;\n\
         \t\tiifname \"aj-h*\" counter drop\n\
         \t\toifname \"aj-h*\" counter drop\n\
         \t}}\n\
         }}\n"
    )
}

fn install(script: &str) -> std::result::Result<(), String> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "`nft` not found in PATH — install nftables, or set {ENV}=off \
                     and isolate aj-h* interfaces yourself"
                )
            } else {
                format!("cannot run nft: {e}")
            }
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .map_err(|e| format!("writing nft script: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("waiting for nft: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "nft -f failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

static INSTALLED: AtomicBool = AtomicBool::new(false);
static WARNED: AtomicBool = AtomicBool::new(false);
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// Whether our table is (still) in the ruleset. An `nft flush ruleset`
/// or a firewall manager reloading its rules removes it.
fn table_present() -> bool {
    Command::new("nft")
        .args(["list", "table", "inet", "agentjail"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Make sure the jail firewall is in place. `mode` comes from
/// [`JailConfig::firewall`](crate::JailConfig::firewall); `None` reads
/// `AGENTJAIL_FIREWALL`. After the first install, each call checks that
/// the table still exists (one `nft list table`) and reinstalls it if
/// it was removed.
pub(crate) fn ensure(mode: Option<Mode>) -> Result<()> {
    let mode = match mode {
        Some(m) => m,
        None => mode_from_env()?,
    };
    let installed = || INSTALLED.load(Ordering::Acquire) && table_present();
    if mode == Mode::Off || installed() {
        return Ok(());
    }
    let _g = INSTALL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if installed() {
        return Ok(());
    }
    match install(&ruleset(super::PROXY_PORT)) {
        Ok(()) => {
            INSTALLED.store(true, Ordering::Release);
            Ok(())
        }
        Err(msg) if mode == Mode::Auto => {
            if !WARNED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "warning: jail network firewall not installed ({msg}); jails can reach \
                     host services on their gateway address. Set {ENV}=nft to make this fatal."
                );
            }
            Ok(())
        }
        Err(msg) => Err(JailError::Network(std::io::Error::other(format!(
            "jail network firewall: {msg}"
        )))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing() {
        assert_eq!(parse_mode(""), Some(Mode::Auto));
        assert_eq!(parse_mode("AUTO"), Some(Mode::Auto));
        assert_eq!(parse_mode("nft"), Some(Mode::Require));
        assert_eq!(parse_mode("require"), Some(Mode::Require));
        assert_eq!(parse_mode("off"), Some(Mode::Off));
        assert_eq!(parse_mode("iptables"), None);
    }

    #[test]
    fn ruleset_shape() {
        let r = ruleset(8080);
        assert!(r.starts_with("add table inet agentjail\ndelete table inet agentjail\n"));
        assert!(r.contains("tcp dport 8080 fib daddr . iif type local accept"));
        assert!(r.contains("iifname \"aj-h*\" counter drop"));
        assert!(r.contains("oifname \"aj-h*\" counter drop"));
        assert!(r.contains("ct state established,related accept"));
    }

    /// When `nft` exists, the script must be accepted by it (checked with
    /// `--check`, which doesn't modify the ruleset).
    #[test]
    fn ruleset_is_valid_nft() {
        let probe = Command::new("nft").arg("--version").output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skip: nft not available");
            return;
        }
        let mut child = Command::new("nft")
            .args(["--check", "-f", "-"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(ruleset(8080).as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "nft rejected ruleset: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
