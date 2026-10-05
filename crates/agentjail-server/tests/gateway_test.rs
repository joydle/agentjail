//! Gateway resolution tests — cover the three domain target paths:
//!  1. `BackendUrl` (static URL forward — existing behaviour)
//!  2. `VmPort` with a live jail registered in `ActiveJailIps` → 200
//!  3. `VmPort` with no live jail → 503
//!
//! The test harness here is deliberately thin: it stands up a tiny
//! upstream HTTP server on 127.0.0.1 and pretends the loopback address
//! is the "jail IP". That's enough to exercise every gateway code
//! path without needing Linux namespaces / veth — the real kernel-
//! level reachability is covered by `inbound_reach_test` in the
//! agentjail crate.
//!
//! Run with: `cargo test -p agentjail-server --test gateway_test`.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use agentjail_ctl::{
    ActiveJailIps, InMemoryWorkspaceStore, Workspace, WorkspaceDomain, WorkspaceSpec,
    WorkspaceStore,
};
use axum::extract::Request;
use axum::response::IntoResponse;
use axum::routing::any;
use axum::Router;
use axum::http::StatusCode;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// NOTE: `agentjail_server::gateway` is a binary-crate private module,
// so we can't import it. Instead, this test replicates the gateway's
// resolution step with the same types and same registry shape — if
// the types change, this file breaks first, which is the point.

#[path = "../src/gateway.rs"]
mod gateway;
use gateway::{GatewayState, router as gw_router};

struct Stack {
    gateway_addr: SocketAddr,
    upstream_addr: SocketAddr,
    jail_ips: Arc<ActiveJailIps>,
    workspaces: Arc<InMemoryWorkspaceStore>,
    stops: Vec<oneshot::Sender<()>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Stack {
    async fn boot() -> Self {
        // 1. Tiny upstream that echoes its Host header + the path it
        //    received so the test can verify the forward landed right.
        async fn echo(req: Request) -> impl IntoResponse {
            let host = req.headers().get("host")
                .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            let path = req.uri().path_and_query()
                .map(|p| p.to_string()).unwrap_or_else(|| "/".into());
            (StatusCode::OK, format!("host={host} path={path}"))
        }
        let upstream = Router::new().fallback(any(echo));
        let up_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = up_listener.local_addr().unwrap();
        let (up_stop, up_rx) = oneshot::channel::<()>();
        let up_task = tokio::spawn(async move {
            axum::serve(up_listener, upstream)
                .with_graceful_shutdown(async { let _ = up_rx.await; })
                .await.unwrap();
        });

        // 2. Shared registries.
        let jail_ips   = Arc::new(ActiveJailIps::new());
        let workspaces = Arc::new(InMemoryWorkspaceStore::new());

        // 3. Gateway.
        let gw_state = GatewayState::new(
            workspaces.clone(),
            jail_ips.clone(),
        ).unwrap();
        let gw_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = gw_listener.local_addr().unwrap();
        let (gw_stop, gw_rx) = oneshot::channel::<()>();
        let gw_task = tokio::spawn(async move {
            axum::serve(gw_listener, gw_router(gw_state))
                .with_graceful_shutdown(async { let _ = gw_rx.await; })
                .await.unwrap();
        });

        Self {
            gateway_addr,
            upstream_addr,
            jail_ips,
            workspaces,
            stops: vec![gw_stop, up_stop],
            tasks: vec![gw_task, up_task],
        }
    }

    async fn stop(mut self) {
        for s in self.stops.drain(..) { let _ = s.send(()); }
        for t in self.tasks.drain(..) { let _ = t.await; }
    }
}

fn sample_ws(id: &str, domains: Vec<WorkspaceDomain>) -> Workspace {
    Workspace {
        id: id.into(),
        created_at: time::OffsetDateTime::now_utc(),
        deleted_at: None,
        source_dir: std::path::PathBuf::from(format!("/tmp/{id}/src")),
        output_dir: std::path::PathBuf::from(format!("/tmp/{id}/out")),
        config: WorkspaceSpec {
            memory_mb: 512, timeout_secs: 60, cpu_percent: 100,
            max_pids: 64, network_mode: "allowlist".into(),
            network_domains: vec!["example.invalid".into()],
            seccomp: "standard".into(), idle_timeout_secs: 0,
        },
        git_repo: None, git_ref: None, label: None, domains,
        last_exec_at: None, paused_at: None, auto_snapshot: None,
    }
}

#[tokio::test]
async fn backend_url_domain_forwards_to_static_upstream() {
    let stack = Stack::boot().await;
    stack.workspaces.insert(sample_ws("wrk_bu", vec![WorkspaceDomain {
        domain: "static.test".into(),
        backend_url: Some(format!("http://{}", stack.upstream_addr)),
        vm_port: None,
    }])).await.unwrap();

    let body = reqwest::Client::new()
        .get(format!("http://{}/hello?q=1", stack.gateway_addr))
        .header("host", "static.test")
        .send().await.unwrap()
        .text().await.unwrap();
    assert!(body.contains("path=/hello?q=1"), "got {body:?}");

    stack.stop().await;
}

#[tokio::test]
async fn vm_port_domain_resolves_via_active_jail_ips() {
    let stack = Stack::boot().await;

    stack.workspaces.insert(sample_ws("wrk_vm", vec![WorkspaceDomain {
        domain: "live.test".into(),
        backend_url: None,
        vm_port: Some(stack.upstream_addr.port()),
    }])).await.unwrap();

    // Publish the workspace's "jail IP" — use 127.0.0.1 so the gateway
    // resolves http://127.0.0.1:<upstream-port>/ and we hit our
    // loopback echo server.
    stack.jail_ips.insert("wrk_vm", Ipv4Addr::new(127, 0, 0, 1));

    let resp = reqwest::Client::new()
        .get(format!("http://{}/api/whoami", stack.gateway_addr))
        .header("host", "live.test")
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("path=/api/whoami"), "got {body:?}");

    stack.stop().await;
}

#[tokio::test]
async fn vm_port_domain_503s_when_no_live_jail() {
    let stack = Stack::boot().await;

    stack.workspaces.insert(sample_ws("wrk_dead", vec![WorkspaceDomain {
        domain: "dead.test".into(),
        backend_url: None,
        vm_port: Some(3000),
    }])).await.unwrap();
    // Intentionally do NOT publish into jail_ips — no exec in flight.

    let resp = reqwest::Client::new()
        .get(format!("http://{}/anything", stack.gateway_addr))
        .header("host", "dead.test")
        .send().await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "expected 503 when no live jail; got {}",
        resp.status()
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("wrk_dead") && body.contains("3000"),
        "503 message should mention workspace + port; got {body:?}");

    stack.stop().await;
}

#[tokio::test]
async fn unknown_host_returns_404() {
    let stack = Stack::boot().await;

    let resp = reqwest::Client::new()
        .get(format!("http://{}/", stack.gateway_addr))
        .header("host", "nobody.declares.this")
        .send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    stack.stop().await;
}
