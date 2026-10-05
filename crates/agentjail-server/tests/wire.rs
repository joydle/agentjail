//! End-to-end wire tests: boot ctl + phantom proxy + mock upstream in-process
//! and drive the full API contract. Proves the phantom-token invariant from
//! every angle: happy path, revocation, multi-service, isolation, audit.

mod harness;

use harness::Stack;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// 1. Happy path: phantom → real key swap, audit recorded
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_openai_phantom_swaps_to_real_key() {
    let s = Stack::boot(&["openai"]).await;

    let session = s.create_session(&["openai"]).await;
    let phantom = session["env"]["OPENAI_API_KEY"].as_str().unwrap();
    let base = session["env"]["OPENAI_BASE_URL"].as_str().unwrap();
    assert!(phantom.starts_with("phm_"));

    let resp = s.post_with_bearer(&format!("{base}/chat/completions"), phantom, json!({"model":"x"})).await;
    assert_eq!(resp.status(), 200);

    // Upstream received real key, never the phantom.
    let upstream_auth = s.last_upstream_auth();
    assert_eq!(upstream_auth, "Bearer sk-real-openai");
    assert!(!upstream_auth.contains("phm_"));

    // Audit has one entry.
    let audit = s.get_audit().await;
    assert_eq!(audit["total"].as_u64().unwrap(), 1);
    assert_eq!(audit["rows"][0]["service"], "openai");

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. Session deletion revokes phantom tokens instantly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_delete_session_revokes_tokens() {
    let s = Stack::boot(&["openai"]).await;

    let session = s.create_session(&["openai"]).await;
    let sid = session["id"].as_str().unwrap();
    let phantom = session["env"]["OPENAI_API_KEY"].as_str().unwrap();
    let base = session["env"]["OPENAI_BASE_URL"].as_str().unwrap();
    let url = format!("{base}/chat/completions");

    // Works before deletion.
    let resp = s.post_with_bearer(&url, phantom, json!({"model":"x"})).await;
    assert_eq!(resp.status(), 200);

    // Delete the session.
    let del = s.http.delete(format!("{}/v1/sessions/{}", s.ctl_base(), sid))
        .bearer_auth(&s.api_key)
        .send().await.unwrap();
    assert_eq!(del.status(), 204);

    // Phantom token is now dead.
    let resp = s.post_with_bearer(&url, phantom, json!({"model":"x"})).await;
    assert_eq!(resp.status(), 401, "revoked token should be rejected");

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 3. Multi-service session: openai + anthropic
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_multi_service_session() {
    let s = Stack::boot(&["openai", "anthropic"]).await;

    let session = s.create_session(&["openai", "anthropic"]).await;
    let env = session["env"].as_object().unwrap();

    // Both services have phantom tokens.
    let oai_phm = env["OPENAI_API_KEY"].as_str().unwrap();
    let ant_phm = env["ANTHROPIC_API_KEY"].as_str().unwrap();
    assert!(oai_phm.starts_with("phm_"));
    assert!(ant_phm.starts_with("phm_"));
    assert_ne!(oai_phm, ant_phm, "different tokens per service");

    // OpenAI call works.
    let oai_base = env["OPENAI_BASE_URL"].as_str().unwrap();
    let resp = s.post_with_bearer(
        &format!("{oai_base}/chat/completions"), oai_phm, json!({"model":"x"}),
    ).await;
    assert_eq!(resp.status(), 200);

    // Anthropic call works.
    let ant_base = env["ANTHROPIC_BASE_URL"].as_str().unwrap();
    let resp = s.post_with_key(
        &format!("{ant_base}/messages"), ant_phm, json!({"model":"x"}),
    ).await;
    assert_eq!(resp.status(), 200);

    // Cross-service: anthropic token on openai path → 403.
    let resp = s.post_with_bearer(
        &format!("{oai_base}/chat/completions"), ant_phm, json!({"model":"x"}),
    ).await;
    assert_eq!(resp.status(), 403, "cross-service token should be rejected");

    // Audit has 2 successful entries.
    let audit = s.get_audit().await;
    assert_eq!(audit["total"].as_u64().unwrap(), 3); // 2 ok + 1 rejected (403 still logged)

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 4. Unknown / garbage token → 401
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_garbage_token_rejected() {
    let s = Stack::boot(&["openai"]).await;
    let _ = s.create_session(&["openai"]).await;

    let resp = s.post_with_bearer(
        &format!("{}/v1/openai/chat/completions", s.proxy_base()),
        "phm_0000000000000000000000000000000000000000000000000000000000000000",
        json!({"model":"x"}),
    ).await;
    assert_eq!(resp.status(), 401);

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 5. Two sessions are isolated — tokens don't cross
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_sessions_are_isolated() {
    let s = Stack::boot(&["openai"]).await;

    let s1 = s.create_session(&["openai"]).await;
    let s2 = s.create_session(&["openai"]).await;

    let phm1 = s1["env"]["OPENAI_API_KEY"].as_str().unwrap();
    let phm2 = s2["env"]["OPENAI_API_KEY"].as_str().unwrap();
    assert_ne!(phm1, phm2, "each session gets a unique token");

    let base = s1["env"]["OPENAI_BASE_URL"].as_str().unwrap();
    let url = format!("{base}/chat/completions");

    // Both work.
    assert_eq!(s.post_with_bearer(&url, phm1, json!({})).await.status(), 200);
    assert_eq!(s.post_with_bearer(&url, phm2, json!({})).await.status(), 200);

    // Delete session 1.
    let sid1 = s1["id"].as_str().unwrap();
    s.http.delete(format!("{}/v1/sessions/{sid1}", s.ctl_base()))
        .bearer_auth(&s.api_key)
        .send().await.unwrap();

    // Session 1 token dead, session 2 still alive.
    assert_eq!(s.post_with_bearer(&url, phm1, json!({})).await.status(), 401);
    assert_eq!(s.post_with_bearer(&url, phm2, json!({})).await.status(), 200);

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 6. Credential lifecycle: add → use → delete → session fails
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_credential_lifecycle() {
    let s = Stack::boot(&[]).await; // no initial keys

    // No credential → session creation fails.
    let resp = s.http.post(format!("{}/v1/sessions", s.ctl_base()))
        .bearer_auth(&s.api_key)
        .json(&json!({"services": ["openai"]}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 400, "no credential should fail session create");

    // Add credential via ctl.
    let resp = s.http.post(format!("{}/v1/credentials", s.ctl_base()))
        .bearer_auth(&s.api_key)
        .json(&json!({"service": "openai", "secret": "sk-added"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Now session works.
    let session = s.create_session(&["openai"]).await;
    let phantom = session["env"]["OPENAI_API_KEY"].as_str().unwrap();
    let base = session["env"]["OPENAI_BASE_URL"].as_str().unwrap();
    let resp = s.post_with_bearer(
        &format!("{base}/chat/completions"), phantom, json!({}),
    ).await;
    assert_eq!(resp.status(), 200);

    // Upstream got the key we added.
    assert_eq!(s.last_upstream_auth(), "Bearer sk-added");

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 7. Exec: run a command in a session's jail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_exec_returns_stdout() {
    let s = Stack::boot_with_exec(&["openai"]).await;

    let session = s.create_session(&["openai"]).await;
    let sid = session["id"].as_str().unwrap();

    // Run echo inside a real jail
    let http_resp = s.http.post(format!("{}/v1/sessions/{sid}/exec", s.ctl_base()))
        .bearer_auth(&s.api_key)
        .json(&json!({"cmd": "/bin/sh", "args": ["-c", "echo hello-from-jail"]}))
        .send().await.unwrap();
    let status = http_resp.status();
    let body = http_resp.text().await.unwrap();
    assert_eq!(status, 200, "exec failed: {body}");
    let resp: Value = serde_json::from_str(&body).unwrap();

    assert_eq!(resp["exit_code"], 0, "stderr: {}", resp["stderr"]);
    assert!(resp["stdout"].as_str().unwrap().contains("hello-from-jail"));
    assert!(!resp["timed_out"].as_bool().unwrap());

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 8. Exec: session env is injected into jail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_exec_injects_phantom_env() {
    let s = Stack::boot_with_exec(&["openai"]).await;

    let session = s.create_session(&["openai"]).await;
    let sid = session["id"].as_str().unwrap();

    // Print the phantom env var from inside the jail
    let resp: Value = s.http.post(format!("{}/v1/sessions/{sid}/exec", s.ctl_base()))
        .bearer_auth(&s.api_key)
        .json(&json!({"cmd": "/bin/sh", "args": ["-c", "echo $OPENAI_API_KEY"]}))
        .send().await.unwrap()
        .json().await.unwrap();

    assert_eq!(resp["exit_code"], 0);
    let stdout = resp["stdout"].as_str().unwrap();
    assert!(stdout.trim().starts_with("phm_"), "jail should see phantom token, got: {stdout}");

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 9. Runs: one-shot code execution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_run_executes_code() {
    let s = Stack::boot_with_exec(&[]).await;

    let resp: Value = s.http.post(format!("{}/v1/runs", s.ctl_base()))
        .bearer_auth(&s.api_key)
        .json(&json!({"code": "echo run-output", "language": "bash"}))
        .send().await.unwrap()
        .json().await.unwrap();

    assert_eq!(resp["exit_code"], 0);
    assert!(resp["stdout"].as_str().unwrap().contains("run-output"));

    s.shutdown().await;
}

// ---------------------------------------------------------------------------
// 10. Exec: timeout enforced
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_run_timeout() {
    let s = Stack::boot_with_exec(&[]).await;

    let resp: Value = s.http.post(format!("{}/v1/runs", s.ctl_base()))
        .bearer_auth(&s.api_key)
        .json(&json!({"code": "sleep 60", "language": "bash", "timeout_secs": 2}))
        .send().await.unwrap()
        .json().await.unwrap();

    assert!(resp["timed_out"].as_bool().unwrap(), "should have timed out");

    s.shutdown().await;
}
