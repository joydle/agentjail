//! Phase P1 spike — can the host reach an HTTP server bound inside a
//! jail spawned with `Network::Allowlist`?
//!
//! This is the foundational question for the App-Builder-style gateway
//! port-forward: if the answer is "yes, naively", then the rest is
//! just plumbing (registry, gateway resolution). If it's "no", we need
//! iptables / NAT work before anything else.
//!
//! The jail's veth peer lives on the same /30 subnet as the host end
//! of the pair, so from the process that spawned the jail, reaching
//! `http://<jail_ip>:<port>/` should Just Work — no NAT, no forwarding
//! rules. This test proves that.
//!
//! Run with: `cargo test --test inbound_reach_test -- --nocapture`.

mod common;

use agentjail::{Jail, Network};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn host_can_reach_jail_bound_http_server() {
    let (src, out) = common::setup("p1", "inbound");

    // Script: start a tiny HTTP server on all interfaces, port 3000.
    // `python3 -m http.server` binds to 0.0.0.0 by default.
    std::fs::write(
        src.join("serve.sh"),
        "#!/bin/sh\nexec python3 -m http.server 3000 --bind 0.0.0.0\n",
    )
    .unwrap();

    let mut config = common::lightweight_config(src.clone(), out.clone());
    // Allowlist mode = veth pair gets created. Domains list is only
    // consulted by the proxy thread; any entry satisfies validation.
    config.network = Network::Allowlist(vec!["example.invalid".into()]);
    // Generous timeout so the server stays alive while we poll.
    config.timeout_secs = 30;

    let jail = Jail::new(config).expect("jail construct");
    let handle = jail.spawn("/bin/sh", &["/workspace/serve.sh"]).expect("jail spawn");

    let jail_ip = handle.jail_ip().expect("allowlist jail should have a jail_ip");
    eprintln!("spike: jail ip = {jail_ip}");

    // Poll until the server accepts. python startup + bind is fast but
    // not instant; give it up to 5 s before declaring a miss.
    let target = format!("{jail_ip}:3000");
    let mut last_err: Option<String> = None;
    let mut status_line: Option<String> = None;
    for attempt in 1..=25 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        match tokio::time::timeout(
            Duration::from_millis(800),
            try_http_get(&target),
        )
        .await
        {
            Ok(Ok(line)) => {
                eprintln!("spike: attempt {attempt} got {line:?}");
                status_line = Some(line);
                break;
            }
            Ok(Err(e)) => {
                last_err = Some(e);
            }
            Err(_) => {
                last_err = Some("connect/read timed out".into());
            }
        }
    }

    // Kill the long-running server regardless of outcome.
    handle.kill();
    common::cleanup(&src, &out);

    match status_line {
        Some(line) => {
            assert!(
                line.starts_with("HTTP/1.0 200") || line.starts_with("HTTP/1.1 200"),
                "expected a 200 from the jail, got {line:?}"
            );
            eprintln!("spike: PASS — host → jail inbound is reachable with no extra plumbing");
        }
        None => {
            // Don't panic loudly — this is a diagnostic spike. Failure
            // here means the gateway port-forward will need NAT rules
            // (not just a registry + backend URL rewrite).
            eprintln!(
                "spike: UNREACHABLE — last error: {}",
                last_err.unwrap_or_else(|| "unknown".into())
            );
            panic!(
                "host could not reach jail {jail_ip}:3000 within 5 s — \
                 gateway design must account for this (iptables DNAT or \
                 equivalent, not just address rewrite)"
            );
        }
    }
}

async fn try_http_get(addr: &str) -> std::result::Result<String, String> {
    let mut s = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    s.write_all(b"GET / HTTP/1.0\r\nHost: spike\r\n\r\n")
        .await
        .map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    let n = s.read(&mut chunk).await.map_err(|e| format!("read: {e}"))?;
    buf.extend_from_slice(&chunk[..n]);
    let text = String::from_utf8_lossy(&buf);
    Ok(text
        .lines()
        .next()
        .unwrap_or("")
        .trim_end_matches('\r')
        .to_string())
}
