//! Shared test harness: boots ctl + phantom proxy + mock upstream.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentjail_ctl::{
    AuditStoreSink, ControlPlane, ControlPlaneConfig, InMemoryAuditStore, InMemoryCredentialStore,
    InMemorySessionStore,
};
use agentjail_phantom::providers::{AnthropicProvider, OpenAiProvider};
use agentjail_phantom::{InMemoryKeyStore, InMemoryTokenStore, PhantomProxy, SecretString, ServiceId};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::any;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Default, Clone)]
struct LastHeaders(Arc<Mutex<Option<HeaderMap>>>);

async fn echo_handler(State(last): State<LastHeaders>, req: Request<Body>) -> impl IntoResponse {
    *last.0.lock().unwrap() = Some(req.headers().clone());
    (StatusCode::OK, [("content-type", "application/json")], r#"{"ok":true}"#)
}

/// Full platform stack running in-process.
pub struct Stack {
    pub http: reqwest::Client,
    pub api_key: String,
    last_headers: LastHeaders,
    ctl_addr: SocketAddr,
    proxy_addr: SocketAddr,
    ctl_stop: Option<oneshot::Sender<()>>,
    proxy_stop: Option<oneshot::Sender<()>>,
    upstream_stop: Option<oneshot::Sender<()>>,
    ctl_task: Option<tokio::task::JoinHandle<()>>,
    proxy_task: Option<tokio::task::JoinHandle<()>>,
    upstream_task: Option<tokio::task::JoinHandle<()>>,
}

impl Stack {
    /// Boot the full stack with exec enabled (for jail tests).
    pub async fn boot_with_exec(initial_keys: &[&str]) -> Self {
        Self::boot_inner(initial_keys, Some(agentjail_ctl::ExecConfig::default())).await
    }

    /// Boot the full stack. `initial_keys` lists services to pre-seed with
    /// real credentials (e.g. `&["openai", "anthropic"]`).
    pub async fn boot(initial_keys: &[&str]) -> Self {
        Self::boot_inner(initial_keys, None).await
    }

    async fn boot_inner(initial_keys: &[&str], exec: Option<agentjail_ctl::ExecConfig>) -> Self {
        let last = LastHeaders::default();

        // 1. Mock upstream that captures headers.
        let app = Router::new()
            .route("/v1/chat/completions", any(echo_handler))
            .route("/chat/completions", any(echo_handler))
            .route("/messages", any(echo_handler))
            .with_state(last.clone());
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let (upstream_stop, upstream_stop_rx) = oneshot::channel::<()>();
        let upstream_task = tokio::spawn(async move {
            axum::serve(upstream_listener, app)
                .with_graceful_shutdown(async { let _ = upstream_stop_rx.await; })
                .await.unwrap();
        });

        // 2. Shared stores.
        let tokens = Arc::new(InMemoryTokenStore::new());
        let keys = Arc::new(InMemoryKeyStore::new());
        let audit = Arc::new(InMemoryAuditStore::new());

        for svc in initial_keys {
            let (id, secret) = match *svc {
                "openai" => (ServiceId::OpenAi, "sk-real-openai"),
                "anthropic" => (ServiceId::Anthropic, "sk-ant-real"),
                "github" => (ServiceId::GitHub, "ghp-real"),
                "stripe" => (ServiceId::Stripe, "sk_test_real"),
                _ => continue,
            };
            // Server integration harness runs under the `"dev"`
            // tenant — the ControlPlane's unauthenticated-default.
            keys.set("dev", id, SecretString::new(secret));
        }

        // 3. Phantom proxy.
        let mut builder = PhantomProxy::builder();
        builder = builder.provider(Arc::new(
            OpenAiProvider::with_base(format!("http://{upstream_addr}")),
        )).unwrap();
        builder = builder.provider(Arc::new(
            AnthropicProvider::with_base(format!("http://{upstream_addr}")),
        )).unwrap();
        let proxy = builder
            .tokens(tokens.clone())
            .keys(keys.clone())
            .audit(Arc::new(AuditStoreSink::new(audit.clone())))
            .build()
            .unwrap();
        let (proxy_addr_tx, proxy_addr_rx) = oneshot::channel::<SocketAddr>();
        let (proxy_stop, proxy_stop_rx) = oneshot::channel::<()>();
        let proxy_task = tokio::spawn({
            let p = proxy.clone();
            async move {
                p.serve_with_bound_addr(
                    "127.0.0.1:0".parse().unwrap(),
                    proxy_addr_tx,
                    async { let _ = proxy_stop_rx.await; },
                ).await.unwrap();
            }
        });
        let proxy_addr = proxy_addr_rx.await.unwrap();

        // 4. Control plane.
        let api_key = "aj_test_key".to_string();
        let ctl = ControlPlane::with_all_stores(
            ControlPlaneConfig {
                tokens: tokens.clone(),
                keys: keys.clone(),
                proxy_base_url: format!("http://{proxy_addr}"),
                api_keys: vec![format!("{api_key}@test:admin")],
                exec,
                state_dir: None,
                snapshot_pool_dir: None,
                platform: None,
                active_jail_ips: None,
            },
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryCredentialStore::new()),
            audit.clone(),
            Arc::new(agentjail_ctl::InMemoryJailStore::new()),
            Arc::new(agentjail_ctl::InMemoryWorkspaceStore::new()),
            Arc::new(agentjail_ctl::InMemorySnapshotStore::new()),
        );
        let ctl_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ctl_addr = ctl_listener.local_addr().unwrap();
        let (ctl_stop, ctl_stop_rx) = oneshot::channel::<()>();
        let router = ctl.router();
        let ctl_task = tokio::spawn(async move {
            axum::serve(ctl_listener, router)
                .with_graceful_shutdown(async { let _ = ctl_stop_rx.await; })
                .await.unwrap();
        });

        // 5s is too tight for jail-spawn round trips on slower hosts
        // (Docker-on-macOS can take 2–3s just to fork a namespaced
        // process). 30s leaves plenty of headroom without masking a
        // real hang — the inner jail timeouts (2s / 30s) fire first.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build().unwrap();

        Self {
            http, api_key, last_headers: last,
            ctl_addr, proxy_addr,
            ctl_stop: Some(ctl_stop),
            proxy_stop: Some(proxy_stop),
            upstream_stop: Some(upstream_stop),
            ctl_task: Some(ctl_task),
            proxy_task: Some(proxy_task),
            upstream_task: Some(upstream_task),
        }
    }

    pub fn ctl_base(&self) -> String { format!("http://{}", self.ctl_addr) }
    pub fn proxy_base(&self) -> String { format!("http://{}", self.proxy_addr) }

    pub async fn create_session(&self, services: &[&str]) -> Value {
        let svcs: Vec<&str> = services.to_vec();
        self.http.post(format!("{}/v1/sessions", self.ctl_base()))
            .bearer_auth(&self.api_key)
            .json(&json!({"services": svcs}))
            .send().await.unwrap()
            .json().await.unwrap()
    }

    pub async fn post_with_bearer(&self, url: &str, token: &str, body: Value) -> reqwest::Response {
        self.http.post(url)
            .bearer_auth(token)
            .json(&body)
            .send().await.unwrap()
    }

    pub async fn post_with_key(&self, url: &str, key: &str, body: Value) -> reqwest::Response {
        self.http.post(url)
            .header("x-api-key", key)
            .json(&body)
            .send().await.unwrap()
    }

    pub fn last_upstream_auth(&self) -> String {
        let h = self.last_headers.0.lock().unwrap();
        let hdrs = h.as_ref().expect("no upstream request received");
        // Check both Authorization and x-api-key
        if let Some(v) = hdrs.get("authorization") {
            v.to_str().unwrap().to_string()
        } else if let Some(v) = hdrs.get("x-api-key") {
            v.to_str().unwrap().to_string()
        } else {
            panic!("no auth header on upstream request")
        }
    }

    pub async fn get_audit(&self) -> Value {
        self.http.get(format!("{}/v1/audit", self.ctl_base()))
            .bearer_auth(&self.api_key)
            .send().await.unwrap()
            .json().await.unwrap()
    }

    pub async fn shutdown(mut self) {
        if let Some(s) = self.ctl_stop.take() { let _ = s.send(()); }
        if let Some(s) = self.proxy_stop.take() { let _ = s.send(()); }
        if let Some(s) = self.upstream_stop.take() { let _ = s.send(()); }
        if let Some(t) = self.ctl_task.take() { let _ = t.await; }
        if let Some(t) = self.proxy_task.take() { let _ = t.await; }
        if let Some(t) = self.upstream_task.take() { let _ = t.await; }
    }
}
