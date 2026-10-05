//! End-to-end integration tests for the phantom proxy.
//!
//! Each test:
//! 1. Spins up a mock upstream on localhost that records the headers it saw.
//! 2. Builds a `PhantomProxy` pointed at that mock.
//! 3. Issues a phantom token and fires a request through the proxy.
//! 4. Asserts the phantom was stripped and the real key arrived at upstream.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentjail_phantom::providers::{AnthropicProvider, OpenAiProvider};
use agentjail_phantom::{
    InMemoryKeyStore, InMemoryTokenStore, PathGlob, PhantomProxy, Scope, SecretString, ServiceId,
    TokenStore,
};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::any;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// A mock upstream that records the last request it received.
#[derive(Default, Clone)]
struct MockUpstream {
    last: Arc<Mutex<Option<RecordedRequest>>>,
}

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: Method,
    path: String,
    query: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl MockUpstream {
    fn take(&self) -> Option<RecordedRequest> {
        self.last.lock().ok()?.take()
    }
}

async fn record_handler(State(m): State<MockUpstream>, req: Request<Body>) -> impl IntoResponse {
    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
    let rec = RecordedRequest {
        method: parts.method,
        path: parts.uri.path().to_string(),
        query: parts.uri.query().unwrap_or_default().to_string(),
        headers: parts.headers,
        body: body_bytes.to_vec(),
    };
    *m.last.lock().unwrap() = Some(rec);
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"ok":true}"#,
    )
}

async fn stream_sse_handler() -> impl IntoResponse {
    let body = "data: hello\n\ndata: world\n\n";
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        body,
    )
}

struct Harness {
    proxy_url: String,
    mock: MockUpstream,
    tokens: Arc<InMemoryTokenStore>,
    shutdowns: Vec<oneshot::Sender<()>>,
    joins: Vec<tokio::task::JoinHandle<()>>,
}

impl Harness {
    async fn openai_with_key(real_key: &str) -> Self {
        let mock = MockUpstream::default();

        // Spin up a mock "openai" upstream.
        let app = Router::new()
            .route("/v1/chat/completions", any(record_handler))
            .route("/v1/files", any(record_handler))
            .route("/v1/stream", any(stream_sse_handler))
            .route("/messages", any(record_handler))
            .with_state(mock.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = listener.local_addr().unwrap();
        let (up_tx, up_rx) = oneshot::channel::<()>();
        let j1 = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = up_rx.await;
                })
                .await
                .unwrap();
        });

        // Build the phantom proxy pointed at the mock.
        let tokens = Arc::new(InMemoryTokenStore::new());
        let keys = Arc::new(InMemoryKeyStore::new());
        // Integration tests run under the `"dev"` tenant, which is
        // also what the harness passes into `tokens.issue(...)` below.
        keys.set("dev", ServiceId::OpenAi,    SecretString::new(real_key));
        keys.set("dev", ServiceId::Anthropic, SecretString::new(real_key));

        let proxy = PhantomProxy::builder()
            .provider(Arc::new(OpenAiProvider::with_base(format!(
                "http://{upstream_addr}"
            ))))
            .unwrap()
            .provider(Arc::new(AnthropicProvider::with_base(format!(
                "http://{upstream_addr}"
            ))))
            .unwrap()
            .tokens(tokens.clone())
            .keys(keys)
            .upstream_timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let (pr_tx, pr_rx) = oneshot::channel::<SocketAddr>();
        let (proxy_shutdown_tx, proxy_shutdown_rx) = oneshot::channel::<()>();
        let p = proxy.clone();
        let j2 = tokio::spawn(async move {
            p.serve_with_bound_addr("127.0.0.1:0".parse().unwrap(), pr_tx, async move {
                let _ = proxy_shutdown_rx.await;
            })
            .await
            .unwrap();
        });
        let proxy_addr = pr_rx.await.unwrap();

        Self {
            proxy_url: format!("http://{proxy_addr}"),
            mock,
            tokens,
            shutdowns: vec![up_tx, proxy_shutdown_tx],
            joins: vec![j1, j2],
        }
    }

    async fn shutdown(self) {
        for tx in self.shutdowns {
            let _ = tx.send(());
        }
        for j in self.joins {
            let _ = j.await;
        }
    }
}

#[tokio::test]
async fn strips_phantom_and_injects_real_key_openai() {
    let h = Harness::openai_with_key("sk-real-openai").await;
    let token = h
        .tokens
        .issue("sess_1".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
        .await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/openai/v1/chat/completions", h.proxy_url))
        .bearer_auth(token.to_string())
        .json(&serde_json::json!({"model": "gpt-4o-mini"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let got = h.mock.take().expect("upstream received a request");
    // Phantom must be gone.
    let auth = got.headers.get("authorization").unwrap().to_str().unwrap();
    assert_eq!(auth, "Bearer sk-real-openai");
    assert!(!auth.contains("phm_"), "phantom must not leak to upstream");

    // Path was rewritten correctly.
    assert_eq!(got.path, "/v1/chat/completions");
    assert_eq!(got.method, Method::POST);

    // Body forwarded intact.
    assert!(
        std::str::from_utf8(&got.body)
            .unwrap()
            .contains("gpt-4o-mini")
    );

    h.shutdown().await;
}

#[tokio::test]
async fn rejects_unknown_token_401() {
    let h = Harness::openai_with_key("sk-real").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/openai/v1/chat/completions", h.proxy_url))
        .bearer_auth("phm_".to_string() + &"a".repeat(64))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);
    assert!(h.mock.take().is_none(), "upstream must not be contacted");
    h.shutdown().await;
}

#[tokio::test]
async fn rejects_missing_token_401() {
    let h = Harness::openai_with_key("sk-real").await;
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/openai/v1/chat/completions", h.proxy_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    assert!(h.mock.take().is_none());
    h.shutdown().await;
}

#[tokio::test]
async fn rejects_wrong_service_for_token_403() {
    let h = Harness::openai_with_key("sk-real").await;
    let token = h
        .tokens
        .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
        .await;

    // Token is for openai but the request is addressed to anthropic.
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/anthropic/messages", h.proxy_url))
        .bearer_auth(token.to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 403);
    assert!(h.mock.take().is_none());
    h.shutdown().await;
}

#[tokio::test]
async fn rejects_out_of_scope_path_403() {
    let h = Harness::openai_with_key("sk-real").await;
    let scope = Scope {
        allowed_paths: vec![PathGlob::new("/v1/chat/*")],
    };
    let token = h
        .tokens
        .issue("s".into(), "dev".into(), ServiceId::OpenAi, scope, None)
        .await;

    // /v1/files is outside the allowed-paths glob.
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/openai/v1/files", h.proxy_url))
        .bearer_auth(token.to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 403);
    assert!(h.mock.take().is_none());

    // But /v1/chat/completions must still work.
    let token2 = h
        .tokens
        .issue(
            "s".into(),
            "dev".into(),
            ServiceId::OpenAi,
            Scope {
                allowed_paths: vec![PathGlob::new("/v1/chat/*")],
            },
            None,
        )
        .await;
    let ok = reqwest::Client::new()
        .post(format!("{}/v1/openai/v1/chat/completions", h.proxy_url))
        .bearer_auth(token2.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);

    h.shutdown().await;
}

#[tokio::test]
async fn anthropic_injects_x_api_key_and_version() {
    let h = Harness::openai_with_key("sk-ant-real").await;
    let token = h
        .tokens
        .issue("s".into(), "dev".into(), ServiceId::Anthropic, Scope::any(), None)
        .await;

    reqwest::Client::new()
        .post(format!("{}/v1/anthropic/messages", h.proxy_url))
        .bearer_auth(token.to_string())
        .json(&serde_json::json!({"model": "claude-opus-4-7"}))
        .send()
        .await
        .unwrap();

    let got = h.mock.take().unwrap();
    assert_eq!(
        got.headers.get("x-api-key").unwrap().to_str().unwrap(),
        "sk-ant-real"
    );
    assert!(got.headers.get("authorization").is_none());
    assert_eq!(
        got.headers
            .get("anthropic-version")
            .unwrap()
            .to_str()
            .unwrap(),
        "2023-06-01"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn revoked_token_401() {
    let h = Harness::openai_with_key("sk").await;
    let token = h
        .tokens
        .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
        .await;
    h.tokens.revoke(&token).await;

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/openai/v1/chat/completions", h.proxy_url))
        .bearer_auth(token.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    h.shutdown().await;
}

#[tokio::test]
async fn streaming_response_is_forwarded() {
    let h = Harness::openai_with_key("sk").await;
    let token = h
        .tokens
        .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
        .await;

    let resp = reqwest::Client::new()
        .get(format!("{}/v1/openai/v1/stream", h.proxy_url))
        .bearer_auth(token.to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/event-stream"
    );
    let text = resp.text().await.unwrap();
    assert!(text.contains("data: hello"));
    assert!(text.contains("data: world"));

    h.shutdown().await;
}

#[tokio::test]
async fn query_string_is_preserved() {
    let h = Harness::openai_with_key("sk").await;
    let token = h
        .tokens
        .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
        .await;

    reqwest::Client::new()
        .get(format!(
            "{}/v1/openai/v1/files?purpose=x&limit=5",
            h.proxy_url
        ))
        .bearer_auth(token.to_string())
        .send()
        .await
        .unwrap();

    let got = h.mock.take().unwrap();
    assert_eq!(got.query, "purpose=x&limit=5");
    h.shutdown().await;
}

#[tokio::test]
async fn unknown_service_path_404() {
    let h = Harness::openai_with_key("sk").await;
    let token = h
        .tokens
        .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
        .await;

    let resp = reqwest::Client::new()
        .get(format!("{}/v1/stripe/customers", h.proxy_url))
        .bearer_auth(token.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    h.shutdown().await;
}

#[tokio::test]
async fn healthz_is_public() {
    let h = Harness::openai_with_key("sk").await;
    let resp = reqwest::get(format!("{}/healthz", h.proxy_url))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    h.shutdown().await;
}

#[tokio::test]
async fn missing_upstream_key_502() {
    let mock = MockUpstream::default();
    let app = Router::new()
        .route("/v1/chat/completions", any(record_handler))
        .with_state(mock.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let (up_tx, up_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = up_rx.await;
            })
            .await
            .unwrap();
    });

    let tokens = Arc::new(InMemoryTokenStore::new());
    let keys = Arc::new(InMemoryKeyStore::new()); // deliberately empty
    let proxy = PhantomProxy::builder()
        .provider(Arc::new(OpenAiProvider::with_base(format!(
            "http://{upstream_addr}"
        ))))
        .unwrap()
        .tokens(tokens.clone())
        .keys(keys)
        .build()
        .unwrap();
    let (pr_tx, pr_rx) = oneshot::channel::<SocketAddr>();
    let (sh_tx, sh_rx) = oneshot::channel::<()>();
    let p = proxy.clone();
    tokio::spawn(async move {
        p.serve_with_bound_addr("127.0.0.1:0".parse().unwrap(), pr_tx, async move {
            let _ = sh_rx.await;
        })
        .await
        .unwrap();
    });
    let proxy_addr = pr_rx.await.unwrap();

    let token = tokens
        .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
        .await;
    let resp = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/openai/v1/chat/completions"))
        .bearer_auth(token.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    assert!(mock.take().is_none(), "upstream must not be contacted");

    let _ = sh_tx.send(());
    let _ = up_tx.send(());
}
