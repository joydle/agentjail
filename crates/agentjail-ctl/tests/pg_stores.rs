//! Postgres-backed store tests.
//!
//! Skipped (pass trivially, with a note on stderr) unless
//! `AJ_TEST_DATABASE_URL` points at a disposable database, e.g.:
//!
//! ```sh
//! docker network create ajnet
//! docker run -d --name ajpg --network ajnet -e POSTGRES_PASSWORD=t postgres:16-alpine
//! AJ_TEST_DATABASE_URL=postgres://postgres:t@ajpg:5432/postgres \
//!   cargo test -p agentjail-ctl --test pg_stores -- --test-threads=1
//! ```
//!
//! Every test uses random ids / hostnames, so reruns against the same
//! database don't collide.

use std::path::PathBuf;
use std::time::Duration;

use agentjail_ctl::{
    CtlError, PgSnapshotStore, PgTokenStore, PgWorkspaceStore, Postgres, SnapshotRecord,
    SnapshotStore, Workspace, WorkspaceDomain, WorkspaceSpec, WorkspaceStore,
};
use agentjail_phantom::{PhantomToken, Scope, ServiceId, TokenStore};
use sqlx::Row;
use time::OffsetDateTime;

async fn pg() -> Option<Postgres> {
    let url = std::env::var("AJ_TEST_DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())?;
    Some(Postgres::connect(&url).await.expect("connect + migrate"))
}

macro_rules! pg_or_skip {
    () => {
        match pg().await {
            Some(p) => p,
            None => {
                eprintln!("AJ_TEST_DATABASE_URL unset; skipping Postgres test");
                return;
            }
        }
    };
}

fn rand_hex() -> String {
    use rand::RngCore;
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn ws(id: &str, tenant: &str, domains: Vec<WorkspaceDomain>) -> Workspace {
    Workspace {
        id: id.into(),
        tenant_id: tenant.into(),
        created_at: OffsetDateTime::now_utc(),
        deleted_at: None,
        source_dir: PathBuf::from(format!("/tmp/{id}/source")),
        output_dir: PathBuf::from(format!("/tmp/{id}/output")),
        config: WorkspaceSpec {
            memory_mb: 512,
            timeout_secs: 60,
            cpu_percent: 100,
            max_pids: 64,
            network_mode: "none".into(),
            network_domains: vec![],
            seccomp: "standard".into(),
            idle_timeout_secs: 0,
            flavors: vec![],
        },
        git_repo: None,
        git_ref: None,
        label: None,
        domains,
        last_exec_at: None,
        paused_at: None,
        auto_snapshot: None,
    }
}

fn dom(d: &str) -> WorkspaceDomain {
    WorkspaceDomain {
        domain: d.into(),
        backend_url: None,
        vm_port: Some(3000),
    }
}

#[tokio::test]
async fn pg_domain_is_unique_across_tenants() {
    let pg = pg_or_skip!();
    let store = PgWorkspaceStore::new(pg.pool.clone());
    let r = rand_hex();
    let host = format!("app-{r}.example.test");
    let (a, b, c) = (
        format!("wrk_a{r}"),
        format!("wrk_b{r}"),
        format!("wrk_c{r}"),
    );

    store
        .insert(ws(&a, "acme", vec![dom(&host)]))
        .await
        .unwrap();

    // Second tenant claiming the same hostname (different case) → 409,
    // and the whole insert is rolled back.
    let err = store
        .insert(ws(&b, "evil", vec![dom(&host.to_uppercase())]))
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::Conflict(_)), "got {err:?}");
    assert!(
        store.get(&b).await.is_none(),
        "conflicting workspace row must be rolled back"
    );

    let (owner, d) = store.by_domain(&host).await.unwrap();
    assert_eq!(owner.id, a);
    assert_eq!(d.domain, host);

    // Releasing on delete makes the hostname claimable again.
    store.mark_deleted(&a).await.unwrap();
    assert!(store.by_domain(&host).await.is_none());
    store
        .insert(ws(&c, "evil", vec![dom(&host)]))
        .await
        .unwrap();
    assert_eq!(store.by_domain(&host).await.unwrap().0.id, c);
}

#[tokio::test]
async fn pg_tokens_are_stored_hashed() {
    let pg = pg_or_skip!();
    let r = rand_hex();
    let session = format!("sess_{r}");
    sqlx::query(
        "INSERT INTO sessions (id, tenant_id, services, env) VALUES ($1, 'dev', '[]', '{}')",
    )
    .bind(&session)
    .execute(&pg.pool)
    .await
    .unwrap();

    let store = PgTokenStore::new(pg.pool.clone());
    let tok = store
        .issue(
            session.clone(),
            "dev".into(),
            ServiceId::OpenAi,
            Scope::any(),
            Some(Duration::from_secs(600)),
        )
        .await;

    // The raw token bytes must not be in the table; the sha256 must be.
    let raw: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM phantom_tokens WHERE token_hash = $1")
        .bind(tok.as_bytes().as_slice())
        .fetch_one(&pg.pool)
        .await
        .unwrap();
    assert_eq!(raw, 0, "raw token stored in DB");
    let digest = {
        use sha2::{Digest, Sha256};
        Sha256::digest(tok.as_bytes()).to_vec()
    };
    let row = sqlx::query("SELECT token_hashed FROM phantom_tokens WHERE token_hash = $1")
        .bind(&digest)
        .fetch_one(&pg.pool)
        .await
        .unwrap();
    assert!(row.get::<bool, _>("token_hashed"));

    let rec = store.lookup(&tok).await.expect("hashed token resolves");
    assert_eq!(rec.session_id, session);
    assert!(rec.expires_at.is_some());

    // Re-running migrations (every boot does) must not re-hash rows.
    let url = std::env::var("AJ_TEST_DATABASE_URL").unwrap();
    let again = Postgres::connect(&url).await.unwrap();
    let store2 = PgTokenStore::new(again.pool.clone());
    assert!(
        store2.lookup(&tok).await.is_some(),
        "migration replay double-hashed tokens"
    );

    store2.revoke(&tok).await;
    assert!(store2.lookup(&tok).await.is_none());
    assert!(store2.lookup(&PhantomToken::generate()).await.is_none());
}

#[tokio::test]
async fn pg_expired_tokens_do_not_resolve() {
    let pg = pg_or_skip!();
    let session = format!("sess_{}", rand_hex());
    sqlx::query(
        "INSERT INTO sessions (id, tenant_id, services, env) VALUES ($1, 'dev', '[]', '{}')",
    )
    .bind(&session)
    .execute(&pg.pool)
    .await
    .unwrap();
    let store = PgTokenStore::new(pg.pool.clone());
    let tok = store
        .issue(
            session,
            "dev".into(),
            ServiceId::OpenAi,
            Scope::any(),
            Some(Duration::from_millis(1)),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(store.lookup(&tok).await.is_none());
}

#[tokio::test]
async fn pg_snapshot_gc_count_is_per_tenant() {
    let pg = pg_or_skip!();
    let store = PgSnapshotStore::new(pg.pool.clone());
    let r = rand_hex();
    let (busy, quiet) = (format!("busy{r}"), format!("quiet{r}"));
    let base = OffsetDateTime::now_utc() - time::Duration::hours(1);
    let rec = |id: String, tenant: &str, offset: i64| SnapshotRecord {
        id,
        tenant_id: tenant.into(),
        workspace_id: None,
        name: None,
        created_at: base + time::Duration::seconds(offset),
        path: PathBuf::from("/nonexistent/agentjail-test"),
        size_bytes: 1,
    };
    store
        .insert(rec(format!("snap_q{r}"), &quiet, 0))
        .await
        .unwrap();
    for i in 0..4 {
        store
            .insert(rec(format!("snap_b{r}_{i}"), &busy, 10 + i))
            .await
            .unwrap();
    }
    let cfg = agentjail_ctl::SnapshotGcConfig {
        max_age_secs: None,
        max_count: Some(2),
        tick_secs: 60,
    };
    agentjail_ctl::snapshot_gc::run_once(&store, &cfg).await;

    assert!(
        store.get(&format!("snap_q{r}")).await.is_some(),
        "quiet tenant's snapshot evicted"
    );
    let (rows, total) = store.list(Some(&busy), None, 100, 0, None).await;
    assert!(total <= 2, "busy tenant kept {total}: {rows:?}");
}
