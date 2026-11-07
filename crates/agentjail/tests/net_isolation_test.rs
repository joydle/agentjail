//! Jail ↔ jail and jail → host isolation on the veth network.
//!
//! Jail A must be able to use its *own* proxy, but must not get an HTTP
//! answer from jail B's proxy (proxy source-address pinning; with the
//! nftables firewall the packets don't even arrive), and must not reach
//! a host service listening on `0.0.0.0` via any host address (needs
//! the nftables firewall: asserted only when `nft` is installed and
//! `AGENTJAIL_FIREWALL` isn't `off`).

mod common;

use agentjail::{Jail, Network};
use std::io::Write;
use std::time::Duration;

const PROBE: &str = r#"
import socket, struct, sys

def gateway():
    with open("/proc/net/route") as f:
        for line in f.readlines()[1:]:
            p = line.split()
            if p[1] == "00000000":
                return socket.inet_ntoa(struct.pack("<L", int(p[2], 16)))

def probe(host, port, send=b""):
    s = socket.socket()
    s.settimeout(2)
    try:
        s.connect((host, port))
        if send:
            s.sendall(send)
        d = s.recv(64)
        return "DATA:" + d.decode(errors="replace").split("\r\n")[0] if d else "EOF"
    except Exception as e:
        return "ERR:" + type(e).__name__
    finally:
        s.close()

gw = gateway()
other = sys.argv[1]
port = int(sys.argv[2])
req = b"CONNECT example.invalid:443 HTTP/1.1\r\n\r\n"
print("own_proxy=" + probe(gw, 8080, req))
print("other_proxy=" + probe(other, 8080, req))
print("host_via_own_gw=" + probe(gw, port))
print("host_via_other_gw=" + probe(other, port))
sys.stdout.flush()
"#;

fn nft_active() -> bool {
    let off = std::env::var("AGENTJAIL_FIREWALL")
        .map(|v| v.eq_ignore_ascii_case("off"))
        .unwrap_or(false);
    !off && std::process::Command::new("nft")
        .args(["list", "table", "inet", "agentjail"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn jail_cannot_reach_other_jail_proxy_or_host_listener() {
    if !common::is_root() {
        eprintln!("skip: needs root");
        return;
    }
    if std::process::Command::new("python3")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("skip: python3 not available");
        return;
    }

    // Host service bound to every address.
    let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    let host_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let _ = s.write_all(b"HOST-LISTENER\n");
        }
    });

    // Jail B just idles; we only need its proxy + gateway address.
    let (src_b, out_b) = common::setup("netiso", "b");
    let mut cfg_b = common::lightweight_config(src_b.clone(), out_b.clone());
    cfg_b.network = Network::Allowlist(vec!["example.invalid".into()]);
    cfg_b.timeout_secs = 60;
    let jail_b = Jail::new(cfg_b).unwrap();
    let hb = jail_b.spawn("/bin/sleep", &["30"]).unwrap();
    let b_ip = hb.jail_ip().unwrap();
    let [x, y, z, _] = b_ip.octets();
    let b_gw = format!("{x}.{y}.{z}.1");

    let (src_a, out_a) = common::setup("netiso", "a");
    std::fs::write(src_a.join("probe.py"), PROBE).unwrap();
    let mut cfg_a = common::lightweight_config(src_a.clone(), out_a.clone());
    cfg_a.network = Network::Allowlist(vec!["example.invalid".into()]);
    cfg_a.timeout_secs = 30;
    let jail_a = Jail::new(cfg_a).unwrap();
    // Firewall (if any) is installed by the first Allowlist spawn above.
    let firewall = nft_active();
    let out = tokio::time::timeout(
        Duration::from_secs(40),
        jail_a.run(
            "/usr/bin/python3",
            &["/workspace/probe.py", &b_gw, &host_port.to_string()],
        ),
    )
    .await
    .expect("probe timed out")
    .unwrap();
    hb.kill();
    let _ = hb.wait().await;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    eprintln!(
        "firewall={firewall}\n{stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let field = |k: &str| {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or_else(|| panic!("missing {k} in probe output"))
            .to_string()
    };

    // Positive control: own proxy answers (allowlisted name that doesn't
    // resolve → 403 from the proxy itself).
    assert!(
        field("own_proxy").starts_with("DATA:HTTP/1.1 403"),
        "own proxy must be reachable: {}",
        field("own_proxy")
    );
    // Jail B's proxy must not serve jail A.
    assert!(
        !field("other_proxy").starts_with("DATA:"),
        "jail A got an answer from jail B's proxy: {}",
        field("other_proxy")
    );

    if firewall {
        for k in ["host_via_own_gw", "host_via_other_gw"] {
            assert!(
                !field(k).starts_with("DATA:"),
                "{k}: jail reached a host 0.0.0.0 listener: {}",
                field(k)
            );
        }
    } else {
        eprintln!("note: nft firewall not active — host-listener isolation not asserted");
    }

    common::cleanup(&src_a, &out_a);
    common::cleanup(&src_b, &out_b);
}

/// The firewall only drops *jail-initiated* traffic to the host: a
/// host-initiated connection into a jail (gateway port-forwarding) must
/// still work, via the `ct state established` rule.
#[tokio::test(flavor = "multi_thread")]
async fn host_initiated_connection_into_jail_still_works() {
    if !common::is_root() {
        eprintln!("skip: needs root");
        return;
    }
    let (src, out) = common::setup("netiso", "inbound");
    std::fs::write(
        src.join("srv.py"),
        "import socket\n\
         s = socket.socket()\n\
         s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n\
         s.bind(('0.0.0.0', 3100))\n\
         s.listen(4)\n\
         print('listening', flush=True)\n\
         while True:\n\
         \x20   c, _ = s.accept()\n\
         \x20   c.sendall(b'FROM-JAIL\\n')\n\
         \x20   c.close()\n",
    )
    .unwrap();
    let mut cfg = common::lightweight_config(src.clone(), out.clone());
    cfg.network = Network::Allowlist(vec!["example.invalid".into()]);
    cfg.timeout_secs = 30;
    let jail = Jail::new(cfg).unwrap();
    let h = jail
        .spawn("/usr/bin/python3", &["/workspace/srv.py"])
        .unwrap();
    let ip = h.jail_ip().unwrap();

    let mut got = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let Ok(Ok(mut s)) = tokio::time::timeout(
            Duration::from_secs(1),
            tokio::net::TcpStream::connect((ip, 3100)),
        )
        .await
        else {
            continue;
        };
        let mut buf = Vec::new();
        use tokio::io::AsyncReadExt;
        if tokio::time::timeout(Duration::from_secs(2), s.read_to_end(&mut buf))
            .await
            .is_ok()
        {
            got = Some(String::from_utf8_lossy(&buf).to_string());
            break;
        }
    }
    h.kill();
    let o = h.wait().await.unwrap();
    eprintln!(
        "jail stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    common::cleanup(&src, &out);
    assert_eq!(
        got.as_deref(),
        Some("FROM-JAIL\n"),
        "host → jail inbound broke"
    );
}
