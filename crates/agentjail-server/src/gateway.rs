//! Hostname-routed HTTP reverse proxy.
//!
//! Listens on `AGENTJAIL_GATEWAY_ADDR` and forwards every incoming
//! request to the backend URL declared by the matching workspace's
//! `domains` list. Minimal on purpose:
//!
//! - HTTP only (no TLS; terminate at an upstream load balancer)
//! - no wildcard matching (exact host compare, case-insensitive)
//! - no caching: the workspace store is queried on every request
//!   (the `by_domain` query is GIN-indexed in the PG backend)
//! - `backend_url` is supplied by the caller — the gateway does not
//!   discover jail-internal IPs.
//!
//! Unmatched hosts get a 404 with a short explanation.

use std::net::SocketAddr;
use std::sync::Arc;

use agentjail_ctl::{ActiveJailIps, WorkspaceDomainTarget, WorkspaceStore};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;

/// Shared gateway state.
#[derive(Clone)]
pub struct GatewayState {
    workspaces: Arc<dyn WorkspaceStore>,
    jail_ips: Arc<ActiveJailIps>,
    client: reqwest::Client,
}

impl GatewayState {
    pub fn new(
        workspaces: Arc<dyn WorkspaceStore>,
        jail_ips: Arc<ActiveJailIps>,
    ) -> anyhow::Result<Self> {
        // Keep the client pooled + long-lived; each backend domain is
        // likely to be reused many times per workspace lifetime.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { workspaces, jail_ips, client })
    }
}

/// Build the axum router. One catch-all route that proxies by `Host`.
pub fn router(state: GatewayState) -> Router {
    Router::new().fallback(any(proxy)).with_state(state)
}

#[tracing::instrument(
    name = "gateway.proxy",
    skip_all,
    fields(host = tracing::field::Empty, workspace_id = tracing::field::Empty, method = %req.method()),
)]
async fn proxy(State(state): State<GatewayState>, req: Request) -> Response {
    let span = tracing::Span::current();
    let Some(host) = extract_host(req.headers()) else {
        return bad_request("missing Host header");
    };
    span.record("host", host.as_str());
    let Some((ws, domain)) = state.workspaces.by_domain(&host).await else {
        tracing::info!(%host, "gateway: no workspace declares host");
        return not_found(&host);
    };
    span.record("workspace_id", ws.id.as_str());

    let uri = req.uri();
    let path_and_query = uri
        .path_and_query()
        .map(http::uri::PathAndQuery::as_str)
        .unwrap_or("/");

    // Resolve the domain target. `BackendUrl` → use it verbatim.
    // `VmPort` → look up the workspace's live jail IP (populated only
    // while an allowlist-networked exec is in flight). Returns 503 when
    // no exec is running — the dev server hasn't started / already
    // exited.
    let forward_url = match domain.target() {
        Ok(WorkspaceDomainTarget::BackendUrl(url)) => {
            format!("{}{}", url.trim_end_matches('/'), path_and_query)
        }
        Ok(WorkspaceDomainTarget::VmPort(port)) => {
            let Some(ip) = state.jail_ips.get(&ws.id) else {
                tracing::info!(
                    %host, workspace_id = %ws.id, port,
                    "gateway: no live jail for vm_port target"
                );
                return no_live_jail(&ws.id, port);
            };
            format!("http://{ip}:{port}{path_and_query}")
        }
        Err(e) => {
            tracing::warn!(
                %host, workspace_id = %ws.id, error = %e,
                "gateway: workspace domain is invalid"
            );
            return bad_gateway(&format!("invalid workspace domain: {e}"));
        }
    };

    let method = req.method().clone();
    let headers = req.headers().clone();
    let body = axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await;
    let body = match body {
        Ok(b) => b,
        Err(e) => return bad_request(&format!("body read: {e}")),
    };

    let mut upstream = state
        .client
        .request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
            &forward_url,
        )
        .body(body);

    for (name, value) in headers.iter() {
        // Hop-by-hop headers are stripped — see RFC 7230 §6.1.
        if is_hop_by_hop(name.as_str()) {
            continue;
        }
        if let Ok(n) = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes())
            && let Ok(v) = reqwest::header::HeaderValue::from_bytes(value.as_bytes())
        {
            upstream = upstream.header(n, v);
        }
    }
    // `extract_host` normalizes the Host header to lowercase + strips
    // the port, so only ASCII-letters/digits/`-`/`.` should remain —
    // all valid HeaderValue bytes. Panic here would mean a broken
    // invariant upstream, not user input.
    let fwd_host = reqwest::header::HeaderValue::from_str(&host)
        .expect("extract_host returns header-safe bytes");
    upstream = upstream.header(
        reqwest::header::HeaderName::from_static("x-forwarded-host"),
        fwd_host,
    );

    let resp = match upstream.send().await {
        Ok(r) => r,
        Err(e) => return bad_gateway(&e.to_string()),
    };

    let status = resp.status().as_u16();
    let mut out_headers = HeaderMap::new();
    for (name, value) in resp.headers().iter() {
        if is_hop_by_hop(name.as_str()) {
            continue;
        }
        if let Ok(n) = http::HeaderName::from_bytes(name.as_str().as_bytes())
            && let Ok(v) = http::HeaderValue::from_bytes(value.as_bytes())
        {
            out_headers.insert(n, v);
        }
    }

    let bytes_result = resp.bytes().await;
    let bytes = match bytes_result {
        Ok(b) => b,
        Err(e) => return bad_gateway(&format!("upstream read: {e}")),
    };

    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() =
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    *response.headers_mut() = out_headers;
    response
}

const MAX_BODY_BYTES: usize = 10 * 1024 * 1024; // 10 MB

fn extract_host(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(http::header::HOST)?.to_str().ok()?;
    // Trim the optional `:port` suffix — hostname routing compares host
    // labels only.
    let host = raw.split(':').next().unwrap_or(raw).trim();
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        format!("gateway: bad request: {msg}\n"),
    )
        .into_response()
}

fn not_found(host: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        format!("gateway: no workspace declares host {host}\n"),
    )
        .into_response()
}

fn bad_gateway(msg: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        format!("gateway: upstream failed: {msg}\n"),
    )
        .into_response()
}

/// 503 response used when a `VmPort` domain has no running exec
/// — i.e. the dev server inside the jail hasn't started yet (or has
/// already exited). Distinct from 404 (no domain declared at all).
fn no_live_jail(workspace_id: &str, port: u16) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!(
            "gateway: no live jail for workspace {workspace_id} on vm_port {port}\n\
             start a workspace exec that binds :{port} before retrying\n"
        ),
    )
        .into_response()
}

/// Spawn the listener as a background task. Returns the join handle.
pub async fn serve(
    addr: SocketAddr,
    state: GatewayState,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let router = router(state);
    tracing::info!(%addr, "gateway listening");
    let mut rx = shutdown;
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = rx.changed().await;
        })
        .await?;
    Ok(())
}
