//! Reverse-proxy HTTP server.
//!
//! Routes incoming requests by their first path segment:
//! `/<service>/<rest...>` → forwards to
//! `<provider.upstream_base()>/<rest...>`, after stripping the phantom
//! `Authorization` header and re-injecting the real one.
//!
//! Plain HTTP on the listen side (the proxy is expected to live on a
//! host-local veth peer that the jail reaches via netns routing — the
//! wire never leaves the host). TLS is terminated upstream via `reqwest`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::TryStreamExt;
use tokio::net::TcpListener;

use crate::error::{PhantomError, Result};
use crate::keys::KeyStore;
use crate::provider::{Provider, ProviderRegistry, ServiceId};
use crate::token::{PhantomToken, TokenRecord, TokenStore};

/// How long an upstream request can take before we give up.
const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(120);

/// Audit record, emitted via [`AuditSink::record`] for every proxied request.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    /// Session the phantom token belonged to. Empty if the token was rejected.
    pub session_id: String,
    /// Service hit.
    pub service: Option<ServiceId>,
    /// Path the client asked for (without the `/<service>` prefix).
    pub path: String,
    /// HTTP method.
    pub method: String,
    /// Final status code returned to the client.
    pub status: u16,
    /// Reason the proxy rejected the request, if any.
    pub reject_reason: Option<&'static str>,
    /// Time spent waiting for the upstream.
    pub upstream_latency: Option<Duration>,
}

/// Something that records audit entries. Implementors must be cheap: the
/// request handler awaits this call before returning to the client.
#[async_trait::async_trait]
pub trait AuditSink: Send + Sync + 'static {
    /// Record an entry.
    async fn record(&self, entry: AuditEntry);
}

/// Audit sink that drops everything.
pub struct NoAudit;

#[async_trait::async_trait]
impl AuditSink for NoAudit {
    async fn record(&self, _: AuditEntry) {}
}

/// Audit sink that logs via the `tracing` crate.
pub struct TracingAudit;

#[async_trait::async_trait]
impl AuditSink for TracingAudit {
    async fn record(&self, entry: AuditEntry) {
        tracing::info!(
            session = %entry.session_id,
            service = ?entry.service,
            method = %entry.method,
            path = %entry.path,
            status = entry.status,
            reject = entry.reject_reason.unwrap_or(""),
            upstream_ms = entry.upstream_latency.map_or(0, |d| d.as_millis() as u64),
            "phantom-proxy"
        );
    }
}

/// Immutable proxy state shared across requests.
pub struct PhantomProxy {
    registry: Arc<ProviderRegistry>,
    tokens: Arc<dyn TokenStore>,
    keys: Arc<dyn KeyStore>,
    audit: Arc<dyn AuditSink>,
    upstream: reqwest::Client,
}

impl PhantomProxy {
    /// Begin building a proxy.
    #[must_use]
    pub fn builder() -> PhantomProxyBuilder {
        PhantomProxyBuilder::default()
    }

    /// Bind to `addr` and serve until `shutdown` resolves.
    pub async fn serve(
        self: Arc<Self>,
        addr: SocketAddr,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| PhantomError::Bind { addr, source: e })?;
        let router = self.router();
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(PhantomError::Io)
    }

    /// Bind to `addr`, report the bound address back, and serve. Useful when
    /// the caller asks for port `0` and needs to know what port was chosen.
    pub async fn serve_with_bound_addr(
        self: Arc<Self>,
        addr: SocketAddr,
        report: tokio::sync::oneshot::Sender<SocketAddr>,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| PhantomError::Bind { addr, source: e })?;
        let bound = listener.local_addr().map_err(PhantomError::Io)?;
        let _ = report.send(bound);
        let router = self.router();
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(PhantomError::Io)
    }

    fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/healthz", any(healthz))
            .route("/v1/*rest", any(handle))
            .fallback(any(not_found))
            .with_state(self)
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "route not found")
}

/// Internal: split `/v1/<service>/<rest>` into (service, rest).
fn split_service_path(uri_path: &str) -> Option<(&str, &str)> {
    let after_v1 = uri_path.strip_prefix("/v1/")?;
    match after_v1.find('/') {
        Some(i) => Some((&after_v1[..i], &after_v1[i..])),
        None => Some((after_v1, "/")),
    }
}

/// Entry point: resolve token → provider → forward.
async fn handle(State(state): State<Arc<PhantomProxy>>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();

    let Some((service_seg, upstream_path)) =
        split_service_path(&path).map(|(s, p)| (s.to_string(), p.to_string()))
    else {
        state
            .audit
            .record(AuditEntry {
                session_id: String::new(),
                service: None,
                path: path.clone(),
                method: method.to_string(),
                status: 404,
                reject_reason: Some("bad_path"),
                upstream_latency: None,
            })
            .await;
        return (StatusCode::NOT_FOUND, "unknown service path").into_response();
    };

    // Resolve phantom token.
    let Some(presented) = extract_phantom(req.headers()) else {
        state
            .audit
            .record(AuditEntry {
                session_id: String::new(),
                service: None,
                path: upstream_path.clone(),
                method: method.to_string(),
                status: 401,
                reject_reason: Some("no_token"),
                upstream_latency: None,
            })
            .await;
        return (StatusCode::UNAUTHORIZED, "missing phantom token").into_response();
    };

    let Some(record) = state.tokens.lookup(&presented).await else {
        state
            .audit
            .record(AuditEntry {
                session_id: String::new(),
                service: None,
                path: upstream_path.clone(),
                method: method.to_string(),
                status: 401,
                reject_reason: Some("unknown_token"),
                upstream_latency: None,
            })
            .await;
        return (StatusCode::UNAUTHORIZED, "unknown phantom token").into_response();
    };

    // Resolve provider.
    let Some((id, provider)) = state.registry.find_by_segment(&service_seg) else {
        reject(
            &state,
            &record,
            &upstream_path,
            &method,
            404,
            "unknown_service",
        )
        .await;
        return (StatusCode::NOT_FOUND, "unknown service").into_response();
    };
    if id != record.service {
        reject(
            &state,
            &record,
            &upstream_path,
            &method,
            403,
            "wrong_service",
        )
        .await;
        return (StatusCode::FORBIDDEN, "token not valid for this service").into_response();
    }

    // Enforce scope.
    if !record.scope.allows_path(&upstream_path) {
        reject(
            &state,
            &record,
            &upstream_path,
            &method,
            403,
            "scope_denied",
        )
        .await;
        return (StatusCode::FORBIDDEN, "path not in scope").into_response();
    }

    // Resolve real key scoped to the token's tenant. A token minted
    // for tenant A can't spend tenant B's credential even if their
    // services match — that was the whole point of binding
    // `tenant_id` into the TokenRecord.
    let Some(secret) = state.keys.get(&record.tenant_id, record.service).await else {
        reject(
            &state,
            &record,
            &upstream_path,
            &method,
            502,
            "no_upstream_key",
        )
        .await;
        return (StatusCode::BAD_GATEWAY, "upstream key not configured").into_response();
    };

    // Forward.
    match forward(
        &state,
        provider.as_ref(),
        &secret,
        req,
        &upstream_path,
        &method,
    )
    .await
    {
        Ok(Forwarded {
            response,
            status,
            latency,
        }) => {
            state
                .audit
                .record(AuditEntry {
                    session_id: record.session_id,
                    service: Some(record.service),
                    path: upstream_path,
                    method: method.to_string(),
                    status: status.as_u16(),
                    reject_reason: None,
                    upstream_latency: Some(latency),
                })
                .await;
            response
        }
        Err(e) => {
            reject(
                &state,
                &record,
                &upstream_path,
                &method,
                502,
                "upstream_error",
            )
            .await;
            (StatusCode::BAD_GATEWAY, format!("upstream: {e}")).into_response()
        }
    }
}

async fn reject(
    state: &PhantomProxy,
    record: &TokenRecord,
    path: &str,
    method: &Method,
    status: u16,
    reason: &'static str,
) {
    state
        .audit
        .record(AuditEntry {
            session_id: record.session_id.clone(),
            service: Some(record.service),
            path: path.to_string(),
            method: method.to_string(),
            status,
            reject_reason: Some(reason),
            upstream_latency: None,
        })
        .await;
}

/// Result of a successful forward. The response is already a streaming axum
/// [`Response`]; latency is the time spent waiting for upstream headers.
struct Forwarded {
    response: Response,
    status: StatusCode,
    latency: Duration,
}

/// Forward the request to the upstream and return a streaming response.
async fn forward(
    state: &PhantomProxy,
    provider: &dyn Provider,
    secret: &crate::keys::SecretString,
    req: Request,
    upstream_path: &str,
    method: &Method,
) -> std::result::Result<Forwarded, String> {
    let (parts, body) = req.into_parts();
    let mut headers = parts.headers;

    provider.strip_client_headers(&mut headers);
    provider
        .inject_auth(&mut headers, secret)
        .map_err(|e| e.to_string())?;

    // Build upstream URL: <base><upstream_path>?<query>
    let mut url = String::with_capacity(128);
    url.push_str(provider.upstream_base());
    url.push_str(upstream_path);
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }

    let reqwest_method =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).map_err(|e| e.to_string())?;

    // Translate body to a reqwest-streaming body.
    let body_stream = body.into_data_stream().map_err(std::io::Error::other);
    let upstream_body = reqwest::Body::wrap_stream(body_stream);

    let started = std::time::Instant::now();
    let resp = state
        .upstream
        .request(reqwest_method, &url)
        .headers(headers)
        .body(upstream_body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let latency = started.elapsed();

    let status_u16 = resp.status().as_u16();
    let status = StatusCode::from_u16(status_u16).unwrap_or(StatusCode::BAD_GATEWAY);

    let mut builder = Response::builder().status(status);
    // Copy upstream headers (minus hop-by-hop).
    if let Some(h) = builder.headers_mut() {
        for (k, v) in resp.headers() {
            if is_hop_by_hop(k.as_str()) {
                continue;
            }
            h.insert(k.clone(), v.clone());
        }
    }

    // Stream the upstream body back to the client.
    let byte_stream = resp.bytes_stream();
    let body = Body::from_stream(byte_stream);

    let response = builder.body(body).map_err(|e| e.to_string())?;

    Ok(Forwarded {
        response,
        status,
        latency,
    })
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "proxy-connection"
            | "keep-alive"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-authenticate"
            | "proxy-authorization"
    )
}

fn extract_phantom(headers: &HeaderMap) -> Option<PhantomToken> {
    if let Some(v) = headers.get(http::header::AUTHORIZATION)
        && let Ok(s) = v.to_str()
        && let Some(tok) = PhantomToken::parse(s)
    {
        return Some(tok);
    }
    if let Some(v) = headers.get("x-api-key")
        && let Ok(s) = v.to_str()
        && let Some(tok) = PhantomToken::parse(s)
    {
        return Some(tok);
    }
    None
}

/// Builder for [`PhantomProxy`].
pub struct PhantomProxyBuilder {
    registry: ProviderRegistry,
    tokens: Option<Arc<dyn TokenStore>>,
    keys: Option<Arc<dyn KeyStore>>,
    audit: Arc<dyn AuditSink>,
    upstream_timeout: Duration,
}

impl Default for PhantomProxyBuilder {
    fn default() -> Self {
        Self {
            registry: ProviderRegistry::new(),
            tokens: None,
            keys: None,
            audit: Arc::new(NoAudit),
            upstream_timeout: DEFAULT_UPSTREAM_TIMEOUT,
        }
    }
}

impl PhantomProxyBuilder {
    /// Register a provider.
    pub fn provider(mut self, p: Arc<dyn Provider>) -> Result<Self> {
        self.registry.register(p)?;
        Ok(self)
    }

    /// Supply the token store. Required.
    #[must_use]
    pub fn tokens(mut self, t: Arc<dyn TokenStore>) -> Self {
        self.tokens = Some(t);
        self
    }

    /// Supply the key store. Required.
    #[must_use]
    pub fn keys(mut self, k: Arc<dyn KeyStore>) -> Self {
        self.keys = Some(k);
        self
    }

    /// Supply an audit sink. Defaults to [`NoAudit`].
    #[must_use]
    pub fn audit(mut self, a: Arc<dyn AuditSink>) -> Self {
        self.audit = a;
        self
    }

    /// Override the upstream request timeout. Defaults to 120s.
    #[must_use]
    pub fn upstream_timeout(mut self, d: Duration) -> Self {
        self.upstream_timeout = d;
        self
    }

    /// Finalize.
    pub fn build(self) -> Result<Arc<PhantomProxy>> {
        let tokens = self
            .tokens
            .ok_or_else(|| PhantomError::Config("tokens store required".into()))?;
        let keys = self
            .keys
            .ok_or_else(|| PhantomError::Config("keys store required".into()))?;
        let upstream = reqwest::Client::builder()
            .timeout(self.upstream_timeout)
            .connect_timeout(Duration::from_secs(10))
            .use_rustls_tls()
            .build()
            .map_err(|e| PhantomError::Config(format!("reqwest: {e}")))?;
        Ok(Arc::new(PhantomProxy {
            registry: Arc::new(self.registry),
            tokens,
            keys,
            audit: self.audit,
            upstream,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_service_path_ok() {
        assert_eq!(
            split_service_path("/v1/openai/chat/completions"),
            Some(("openai", "/chat/completions"))
        );
        assert_eq!(split_service_path("/v1/openai"), Some(("openai", "/")));
        assert_eq!(split_service_path("/v1/openai/"), Some(("openai", "/")));
        assert_eq!(split_service_path("/v2/openai/x"), None);
        assert_eq!(split_service_path("/"), None);
    }

    #[test]
    fn hop_by_hop_case_insensitive() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("keep-alive"));
        assert!(is_hop_by_hop("TRANSFER-ENCODING"));
        assert!(!is_hop_by_hop("content-type"));
        assert!(!is_hop_by_hop("x-custom"));
    }

    #[test]
    fn extract_phantom_from_bearer() {
        let t = PhantomToken::generate();
        let s = t.to_string();
        let mut h = HeaderMap::new();
        h.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {s}").parse().unwrap(),
        );
        assert!(extract_phantom(&h).unwrap().ct_eq(&t));
    }

    #[test]
    fn extract_phantom_from_x_api_key() {
        let t = PhantomToken::generate();
        let s = t.to_string();
        let mut h = HeaderMap::new();
        h.insert("x-api-key", s.parse().unwrap());
        assert!(extract_phantom(&h).unwrap().ct_eq(&t));
    }

    #[test]
    fn extract_phantom_none_for_garbage() {
        let mut h = HeaderMap::new();
        h.insert(
            http::header::AUTHORIZATION,
            "Bearer sk-real".parse().unwrap(),
        );
        assert!(extract_phantom(&h).is_none());
    }
}
