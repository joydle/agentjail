//! End-to-end HTTP integration tests for the control plane.

use std::net::SocketAddr;
use std::sync::Arc;

use agentjail_ctl::{ControlPlane, ControlPlaneConfig, ExecConfig};
use agentjail_phantom::{InMemoryKeyStore, InMemoryTokenStore};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

struct Harness {
    base: String,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start(api_keys: Vec<String>) -> Self {
        let api_keys = api_keys
            .into_iter()
            .map(|t| format!("{t}@test:admin"))
            .collect();
        Self::start_structured(api_keys, None).await.0
    }

    /// Start with pre-formatted `token@tenant:role` keys. Returns both
    /// the harness and the state-dir used, so tests that need to plant
    /// things (flavor directories, etc.) know where to reach.
    async fn start_structured(
        api_keys: Vec<String>,
        state_dir: Option<std::path::PathBuf>,
    ) -> (Self, std::path::PathBuf) {
        let tokens = Arc::new(InMemoryTokenStore::new());
        let keys = Arc::new(InMemoryKeyStore::new());
        let state_dir = state_dir.unwrap_or_else(|| tempfile::tempdir().unwrap().keep());
        let ctl = ControlPlane::new(ControlPlaneConfig {
            tokens,
            keys,
            proxy_base_url: "http://10.0.0.1:8443".into(),
            api_keys,
            // Workspace create + patch + delete only need
            // `exec_config` to be `Some` — they read defaults off it
            // without actually spawning a jail. Providing the default
            // here lets us test the HTTP surface without needing the
            // host's sandbox capabilities.
            exec: Some(ExecConfig::default()),
            state_dir: Some(state_dir.clone()),
            snapshot_pool_dir: None,
            platform: None,
            active_jail_ips: None,
        });
        let router = ctl.router();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });
        (
            Self {
                base: format!("http://{addr}"),
                shutdown: Some(tx),
                join,
            },
            state_dir,
        )
    }

    async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().build().unwrap()
}

#[tokio::test]
async fn healthz_is_public() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let r = reqwest::get(format!("{}/healthz", h.base)).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "ok");
    h.stop().await;
}

#[tokio::test]
async fn guarded_routes_require_api_key() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let r = client()
        .get(format!("{}/v1/credentials", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let r = client()
        .get(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    h.stop().await;
}

/// Regression guard — both newly-added endpoints (settings + snapshot
/// manifest) must live on the guarded router, not the public one.
/// Future refactors that accidentally move them will fail this test.
#[tokio::test]
async fn new_endpoints_require_api_key() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    for path in ["/v1/config", "/v1/snapshots/snap_x/manifest"] {
        let no_auth = client().get(format!("{}{path}", h.base)).send().await.unwrap();
        assert_eq!(no_auth.status(), 401, "{path} should 401 without bearer");
        let bad_auth = client()
            .get(format!("{}{path}", h.base))
            .bearer_auth("aj_wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(bad_auth.status(), 401, "{path} should 401 with wrong bearer");
    }
    h.stop().await;
}

#[tokio::test]
async fn attach_list_delete_credential() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let c = client();

    // Attach.
    let r = c
        .post(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "service": "openai", "secret": "sk-abc" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["service"], "openai");
    assert!(!body["fingerprint"].as_str().unwrap().is_empty());
    assert!(body["added_at"].as_str().is_some());

    // Rotate: updated_at should change, added_at should not.
    let added_at_1 = body["added_at"].as_str().unwrap().to_string();
    let r = c
        .post(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "service": "openai", "secret": "sk-rotated" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body2: Value = r.json().await.unwrap();
    assert_eq!(body2["added_at"].as_str().unwrap(), added_at_1);
    assert_ne!(body2["fingerprint"], body["fingerprint"]);

    // List.
    let r = c
        .get(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .send()
        .await
        .unwrap();
    let list: Vec<Value> = r.json().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["service"], "openai");

    // Delete.
    let r = c
        .delete(format!("{}/v1/credentials/openai", h.base))
        .bearer_auth("aj_test")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    let r = c
        .get(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .send()
        .await
        .unwrap();
    let list: Vec<Value> = r.json().await.unwrap();
    assert!(list.is_empty());

    h.stop().await;
}

#[tokio::test]
async fn reject_empty_secret() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let r = client()
        .post(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "service": "openai", "secret": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    h.stop().await;
}

#[tokio::test]
async fn create_session_with_no_credential_fails() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let r = client()
        .post(format!("{}/v1/sessions", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "services": ["openai"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    h.stop().await;
}

#[tokio::test]
async fn session_lifecycle() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let c = client();

    // Configure credentials for two services.
    c.post(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "service": "openai", "secret": "sk-real" }))
        .send()
        .await
        .unwrap();
    c.post(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "service": "anthropic", "secret": "sk-ant" }))
        .send()
        .await
        .unwrap();

    // Create session.
    let r = c
        .post(format!("{}/v1/sessions", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "services": ["openai", "anthropic"], "ttl_secs": 300 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let body: Value = r.json().await.unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("sess_"));
    assert!(body["expires_at"].as_str().is_some());

    // env must contain phantom tokens and proxy base URLs.
    let env = body["env"].as_object().unwrap();
    let openai_key = env["OPENAI_API_KEY"].as_str().unwrap();
    assert!(openai_key.starts_with("phm_"));
    assert_eq!(openai_key.len(), 4 + 64);
    assert_eq!(
        env["OPENAI_BASE_URL"].as_str().unwrap(),
        "http://10.0.0.1:8443/v1/openai/v1"
    );
    let anth_key = env["ANTHROPIC_API_KEY"].as_str().unwrap();
    assert!(anth_key.starts_with("phm_"));
    assert_ne!(anth_key, openai_key, "tokens must be distinct per service");
    assert_eq!(
        env["ANTHROPIC_BASE_URL"].as_str().unwrap(),
        "http://10.0.0.1:8443/v1/anthropic"
    );

    // List sessions.
    let list: Vec<Value> = c
        .get(format!("{}/v1/sessions", h.base))
        .bearer_auth("aj_test")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"].as_str().unwrap(), id);

    // Get a single session.
    let one: Value = c
        .get(format!("{}/v1/sessions/{id}", h.base))
        .bearer_auth("aj_test")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["id"].as_str().unwrap(), id);

    // Delete.
    let r = c
        .delete(format!("{}/v1/sessions/{id}", h.base))
        .bearer_auth("aj_test")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    let r = c
        .get(format!("{}/v1/sessions/{id}", h.base))
        .bearer_auth("aj_test")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    h.stop().await;
}

#[tokio::test]
async fn unknown_service_4xx() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let r = client()
        .post(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "service": "not-a-real-service", "secret": "sk" }))
        .send()
        .await
        .unwrap();
    // Serde rejects the unknown variant -> 422 from axum's Json extractor.
    assert!(r.status().is_client_error());
    h.stop().await;
}

#[tokio::test]
async fn session_with_scopes_reaches_through_to_token() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let c = client();
    c.post(format!("{}/v1/credentials", h.base))
        .bearer_auth("aj_test")
        .json(&json!({ "service": "github", "secret": "ghp_x" }))
        .send()
        .await
        .unwrap();

    // Happy path: valid scope.
    let r = c
        .post(format!("{}/v1/sessions", h.base))
        .bearer_auth("aj_test")
        .json(&json!({
            "services": ["github"],
            "scopes": { "github": ["/repos/foo/*"] }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);

    // Scope keyed by a service not in `services` -> 400.
    let r = c
        .post(format!("{}/v1/sessions", h.base))
        .bearer_auth("aj_test")
        .json(&json!({
            "services": ["github"],
            "scopes": { "openai": ["/v1/*"] }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    h.stop().await;
}

#[tokio::test]
async fn audit_endpoint_returns_empty_initially() {
    let h = Harness::start(vec!["aj_test".into()]).await;
    let body: Value = client()
        .get(format!("{}/v1/audit", h.base))
        .bearer_auth("aj_test")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], 0);
    assert!(body["rows"].as_array().unwrap().is_empty());
    h.stop().await;
}

// ─── tenancy + flavors + whoami ─────────────────────────────────────────

#[tokio::test]
async fn whoami_returns_tenant_and_role() {
    let (h, _) = Harness::start_structured(
        vec!["ak_ops@platform:admin".into(), "ak_ac@acme:operator".into()],
        None,
    ).await;
    let c = client();

    let body: Value = c.get(format!("{}/v1/whoami", h.base))
        .bearer_auth("ak_ops").send().await.unwrap().json().await.unwrap();
    assert_eq!(body["tenant"], "platform");
    assert_eq!(body["role"], "admin");

    let body: Value = c.get(format!("{}/v1/whoami", h.base))
        .bearer_auth("ak_ac").send().await.unwrap().json().await.unwrap();
    assert_eq!(body["tenant"], "acme");
    assert_eq!(body["role"], "operator");

    h.stop().await;
}

#[tokio::test]
async fn operator_workspace_list_is_tenant_scoped() {
    // Two operators in different tenants + one admin. Each operator
    // creates a workspace; admin should see both, operators only
    // their own.
    let (h, _) = Harness::start_structured(
        vec![
            "ak_admin@platform:admin".into(),
            "ak_acme@acme:operator".into(),
            "ak_globex@globex:operator".into(),
        ],
        None,
    ).await;
    let c = client();

    for who in ["ak_acme", "ak_globex"] {
        let r = c.post(format!("{}/v1/workspaces", h.base))
            .bearer_auth(who)
            .json(&json!({ "label": format!("ws-{who}") }))
            .send().await.unwrap();
        assert_eq!(r.status(), 201, "{who} create should succeed");
    }

    // acme operator: sees only its own workspace.
    let body: Value = c.get(format!("{}/v1/workspaces", h.base))
        .bearer_auth("ak_acme").send().await.unwrap().json().await.unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["rows"][0]["label"], "ws-ak_acme");
    assert_eq!(body["rows"][0]["tenant_id"], "acme");
    // Host paths redacted for operators.
    assert_eq!(body["rows"][0]["source_dir"], "");
    assert_eq!(body["rows"][0]["output_dir"], "");

    // globex operator: sees only its own workspace.
    let body: Value = c.get(format!("{}/v1/workspaces", h.base))
        .bearer_auth("ak_globex").send().await.unwrap().json().await.unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["rows"][0]["tenant_id"], "globex");

    // admin: sees both, and keeps host paths.
    let body: Value = c.get(format!("{}/v1/workspaces", h.base))
        .bearer_auth("ak_admin").send().await.unwrap().json().await.unwrap();
    assert_eq!(body["total"], 2);
    assert!(body["rows"][0]["source_dir"].as_str().unwrap().starts_with('/'));

    h.stop().await;
}

#[tokio::test]
async fn operator_cannot_read_other_tenants_workspace_by_id() {
    // Direct-id access across tenants must 404, not 403, so an
    // attacker can't learn whether an id exists on a foreign tenant.
    let (h, _) = Harness::start_structured(
        vec!["ak_a@acme:operator".into(), "ak_b@globex:operator".into()],
        None,
    ).await;
    let c = client();
    let created: Value = c.post(format!("{}/v1/workspaces", h.base))
        .bearer_auth("ak_a")
        .json(&json!({}))
        .send().await.unwrap().json().await.unwrap();
    let wid = created["id"].as_str().unwrap();

    let r = c.get(format!("{}/v1/workspaces/{wid}", h.base))
        .bearer_auth("ak_b").send().await.unwrap();
    assert_eq!(r.status(), 404);

    h.stop().await;
}

#[tokio::test]
async fn unknown_flavor_rejected_on_create() {
    let (h, _) = Harness::start_structured(
        vec!["ak@acme:operator".into()],
        None,
    ).await;
    let c = client();

    let r = c.post(format!("{}/v1/workspaces", h.base))
        .bearer_auth("ak")
        .json(&json!({ "flavors": ["nodejs"] }))
        .send().await.unwrap();
    assert_eq!(r.status(), 400);
    let body: Value = r.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("nodejs"));

    h.stop().await;
}

#[tokio::test]
async fn known_flavor_lands_in_workspace_spec() {
    // Plant a flavor directory, then create a workspace referencing it.
    let state_dir = tempfile::tempdir().unwrap().keep();
    let flavor = state_dir.join("flavors/nodejs");
    std::fs::create_dir_all(flavor.join("bin")).unwrap();

    let (h, _) = Harness::start_structured(
        vec!["ak@acme:operator".into()],
        Some(state_dir),
    ).await;
    let c = client();

    let body: Value = c.post(format!("{}/v1/workspaces", h.base))
        .bearer_auth("ak")
        .json(&json!({ "flavors": ["nodejs"] }))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(body["config"]["flavors"], json!(["nodejs"]));

    h.stop().await;
}

#[tokio::test]
async fn credentials_are_tenant_scoped() {
    // Two operators in different tenants + one admin. Each operator
    // owns credentials for its own tenant; neither can see the other's.
    let (h, _) = Harness::start_structured(
        vec![
            "ak_admin@platform:admin".into(),
            "ak_acme@acme:operator".into(),
            "ak_globex@globex:operator".into(),
        ],
        None,
    ).await;
    let c = client();

    // Each operator adds its own credential.
    for (who, tenant, secret) in [
        ("ak_acme",   "acme",   "sk-acme-key"),
        ("ak_globex", "globex", "sk-globex-key"),
    ] {
        let r = c.post(format!("{}/v1/credentials", h.base))
            .bearer_auth(who)
            .json(&json!({ "service": "openai", "secret": secret }))
            .send().await.unwrap();
        assert_eq!(r.status(), 200, "{who} upsert should succeed");
        let body: Value = r.json().await.unwrap();
        assert_eq!(body["tenant_id"], tenant);
    }

    // Each operator sees only its own row.
    for (who, tenant) in [("ak_acme", "acme"), ("ak_globex", "globex")] {
        let body: Value = c.get(format!("{}/v1/credentials", h.base))
            .bearer_auth(who).send().await.unwrap().json().await.unwrap();
        let rows = body.as_array().unwrap();
        assert_eq!(rows.len(), 1, "{who} sees one row");
        assert_eq!(rows[0]["tenant_id"], tenant);
    }

    // Admin sees both.
    let body: Value = c.get(format!("{}/v1/credentials", h.base))
        .bearer_auth("ak_admin").send().await.unwrap().json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);

    // Admin can target a specific tenant via ?tenant=.
    let body: Value = c.get(format!("{}/v1/credentials?tenant=acme", h.base))
        .bearer_auth("ak_admin").send().await.unwrap().json().await.unwrap();
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["tenant_id"], "acme");

    // Operator passing ?tenant=<other> gets 404 (we don't reveal the
    // other tenant exists).
    let r = c.post(format!("{}/v1/credentials?tenant=globex", h.base))
        .bearer_auth("ak_acme")
        .json(&json!({ "service": "openai", "secret": "sk-evil" }))
        .send().await.unwrap();
    assert_eq!(r.status(), 404);

    // Operator can delete its OWN credential.
    let r = c.delete(format!("{}/v1/credentials/openai", h.base))
        .bearer_auth("ak_acme").send().await.unwrap();
    assert_eq!(r.status(), 204);

    // But can't delete another tenant's, even by id.
    let r = c.delete(format!("{}/v1/credentials/openai?tenant=globex", h.base))
        .bearer_auth("ak_acme").send().await.unwrap();
    assert_eq!(r.status(), 404);

    h.stop().await;
}

#[tokio::test]
async fn from_snapshot_requires_and_checks_parent_workspace_id() {
    let (h, _) = Harness::start_structured(
        vec!["ak_a@acme:operator".into(), "ak_b@globex:operator".into()],
        None,
    ).await;
    let c = client();

    // Request missing `parent_workspace_id` — axum rejects at body
    // extraction (required field), 422. The route handler never runs,
    // which is exactly what we want security-wise.
    let r = c.post(format!("{}/v1/workspaces/from-snapshot", h.base))
        .bearer_auth("ak_a")
        .json(&json!({ "snapshot_id": "snap_nonexistent" }))
        .send().await.unwrap();
    assert_eq!(r.status(), 422);

    // Request with bogus snapshot + workspace id — 404 (no leak of
    // whether the snapshot exists; the snapshot lookup runs first so
    // empty/whitespace parent_workspace_id validation can't be
    // checked here without a real snapshot).
    let r = c.post(format!("{}/v1/workspaces/from-snapshot", h.base))
        .bearer_auth("ak_a")
        .json(&json!({
            "snapshot_id": "snap_nonexistent",
            "parent_workspace_id": "wrk_nonexistent",
        }))
        .send().await.unwrap();
    assert_eq!(r.status(), 404);

    h.stop().await;
}

#[tokio::test]
async fn settings_bind_addrs_hidden_from_operators() {
    let (h, _) = Harness::start_structured(
        vec!["ak_admin@platform:admin".into(), "ak_op@acme:operator".into()],
        None,
    ).await;
    let c = client();

    // Operator: state_dir / bind addrs absent.
    let body: Value = c.get(format!("{}/v1/config", h.base))
        .bearer_auth("ak_op").send().await.unwrap().json().await.unwrap();
    assert!(body["persistence"]["state_dir"].is_null(),
        "state_dir must be absent for operators");
    assert!(body["proxy"]["bind_addr"].is_null(),
        "proxy.bind_addr must be absent for operators");

    // Admin: state_dir present.
    let body: Value = c.get(format!("{}/v1/config", h.base))
        .bearer_auth("ak_admin").send().await.unwrap().json().await.unwrap();
    assert!(body["persistence"]["state_dir"].as_str().is_some(),
        "state_dir must be present for admins");

    h.stop().await;
}

#[tokio::test]
async fn flavors_list_reflects_state_dir_contents() {
    // Plant two flavors + one non-safe name and one plain file.
    // Only the two safe directories should land in the response.
    let state_dir = tempfile::tempdir().unwrap().keep();
    std::fs::create_dir_all(state_dir.join("flavors/nodejs/bin")).unwrap();
    std::fs::create_dir_all(state_dir.join("flavors/python/bin")).unwrap();
    std::fs::create_dir_all(state_dir.join("flavors/BAD-name")).unwrap();
    std::fs::write(state_dir.join("flavors/README.txt"), b"").unwrap();

    let (h, _) = Harness::start_structured(
        vec!["ak@acme:operator".into()],
        Some(state_dir),
    ).await;
    let c = client();

    let body: Value = c.get(format!("{}/v1/flavors", h.base))
        .bearer_auth("ak").send().await.unwrap().json().await.unwrap();
    let names: Vec<&str> = body.as_array().unwrap()
        .iter().map(|v| v["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["nodejs", "python"]);

    // No host path leaked.
    assert!(body[0].get("path").is_none());

    h.stop().await;
}

#[tokio::test]
async fn flavors_list_requires_auth() {
    let (h, _) = Harness::start_structured(
        vec!["ak@acme:operator".into()],
        None,
    ).await;
    let r = client().get(format!("{}/v1/flavors", h.base)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    h.stop().await;
}

#[tokio::test]
async fn rename_workspace_cross_tenant_404() {
    let (h, _) = Harness::start_structured(
        vec!["ak_a@acme:operator".into(), "ak_b@globex:operator".into()],
        None,
    ).await;
    let c = client();
    let created: Value = c.post(format!("{}/v1/workspaces", h.base))
        .bearer_auth("ak_a").json(&json!({})).send().await.unwrap().json().await.unwrap();
    let wid = created["id"].as_str().unwrap();

    let r = c.patch(format!("{}/v1/workspaces/{wid}", h.base))
        .bearer_auth("ak_b")
        .json(&json!({ "label": "pwned" }))
        .send().await.unwrap();
    assert_eq!(r.status(), 404);

    // Owner can rename — and the response redacts host paths.
    let r = c.patch(format!("{}/v1/workspaces/{wid}", h.base))
        .bearer_auth("ak_a")
        .json(&json!({ "label": "mine" }))
        .send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["label"], "mine");
    assert_eq!(body["source_dir"], "");

    h.stop().await;
}
