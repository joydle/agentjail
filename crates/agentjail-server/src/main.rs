//! Runs the phantom proxy + control plane together.
//!
//! Two HTTP listeners are started:
//!  * `PROXY_ADDR` — the sandbox-facing phantom proxy (default 127.0.0.1:8443)
//!  * `CTL_ADDR`   — the operator-facing control plane (default 127.0.0.1:7000)
//!
//! Both share the same in-memory token, key, and audit stores, so
//! phantom tokens issued by the control plane resolve in the proxy, and
//! proxy requests show up in the control plane's `/v1/audit` feed.
//!
//! ## Environment
//!
//! | Var                | Default              | What                       |
//! |--------------------|----------------------|----------------------------|
//! | `CTL_ADDR`         | `127.0.0.1:7000`     | Control plane bind address |
//! | `PROXY_ADDR`       | `127.0.0.1:8443`     | Phantom proxy bind address |
//! | `PROXY_BASE_URL`   | `http://<PROXY_ADDR>`| URL the sandbox uses       |
//! | `AGENTJAIL_API_KEY`| (none → auth off)    | Comma-separated API keys   |
//! | `AGENTJAIL_STATE_DIR` | `$TMPDIR/agentjail-state` | Persistent workspace + snapshot root |
//! | `AGENTJAIL_SNAPSHOT_MAX_AGE_SECS` | — | Drop snapshots older than N sec (GC) |
//! | `AGENTJAIL_SNAPSHOT_MAX_COUNT`    | — | Keep at most N snapshots (GC)        |
//! | `AGENTJAIL_SNAPSHOT_GC_TICK_SECS` | 60 | GC sweep interval                   |
//! | `AGENTJAIL_SNAPSHOT_POOL_DIR`     | — | Content-addressed snapshot pool      |
//! | `AGENTJAIL_IDLE_CHECK_INTERVAL_SECS` | 30 | Workspace idle-reaper interval   |
//! | `AGENTJAIL_GATEWAY_ADDR`          | — | Hostname-routed proxy bind (e.g. 0.0.0.0:8080) |
//! | `OPENAI_API_KEY`   | —                    | Seeded as real key         |
//! | `ANTHROPIC_API_KEY`| —                    | Seeded as real key         |
//! | `GITHUB_TOKEN`     | —                    | Seeded as real key         |
//! | `STRIPE_API_KEY`   | —                    | Seeded as real key         |
//!
//! The `*_KEY` env vars are *only* read by this process and stored in the
//! host's in-memory key store. They are never forwarded to sandboxes —
//! sandboxes receive `phm_<hex>` phantom tokens and the proxy injects the
//! real value on their behalf.

use std::net::SocketAddr;
use std::sync::Arc;

use agentjail_ctl::{
    AuditStore, AuditStoreSink, ControlPlane, ControlPlaneConfig, CredentialStore,
    InMemoryAuditStore, InMemoryCredentialStore, InMemoryJailStore, InMemorySessionStore,
    InMemorySnapshotStore, InMemoryWorkspaceStore, JailStore, PgAuditStore, PgCredentialStore,
    PgJailStore, PgSnapshotStore, PgWorkspaceStore, Postgres, SessionStore, SnapshotGcConfig,
    SnapshotStore, WorkspaceStore, snapshot_gc,
    workspace_idle::{IdleReaperConfig, spawn_sweeper as spawn_idle_sweeper},
};

mod gateway;
use gateway::GatewayState;
use agentjail_phantom::providers::{
    AnthropicProvider, GitHubProvider, OpenAiProvider, StripeProvider,
};
use agentjail_phantom::{
    InMemoryKeyStore, InMemoryTokenStore, LruTokenCache, PhantomProxy, SecretString, ServiceId,
    TokenStore,
};
use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    // Reap any `aj-h*` veth interfaces left behind by a previous crash.
    // `PR_SET_PDEATHSIG` handles the graceful-exit case inside each
    // jail; this covers the "killed -9" / "OOM'd" / "power lost" gap.
    agentjail::cleanup_stale_veths();

    let config = Config::from_env()?;
    let mut stores = Stores::new_from_env();

    // Optional Postgres. When DATABASE_URL is set we hydrate the phantom
    // key store from persisted credentials, route credential/audit/jail
    // writes through the DB, and flip the phantom-token store to a
    // PG-backed + LRU-cached impl so issued tokens survive restarts.
    let pg = match std::env::var("DATABASE_URL").ok().filter(|s| !s.trim().is_empty()) {
        Some(url) => {
            tracing::info!("connecting to postgres");
            let pg = Postgres::connect(&url)
                .await
                .with_context(|| format!("connecting to {url}"))?;
            let rehydrated = pg.rehydrate_keys(&stores.keys).await?;
            tracing::info!(%rehydrated, "postgres ready");

            // Durable tokens: DB is source of truth, LRU cache fronts
            // the proxy's hot-path lookup. Cache size configurable via
            // `AGENTJAIL_TOKEN_CACHE_SIZE`.
            let raw  = agentjail_ctl::PgTokenStore::new(pg.pool.clone());
            let cap  = token_cache_capacity();
            stores.tokens = Arc::new(LruTokenCache::new(raw, cap));
            tracing::info!(capacity = cap, "phantom token store: postgres + LRU cache");
            Some(pg)
        }
        None => {
            tracing::warn!("DATABASE_URL unset — using in-memory stores (state is lost on restart)");
            None
        }
    };

    // Cross-service shutdown signal.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Build the phantom proxy. When Postgres is on, the proxy still reads
    // keys from the in-memory store (hot path) but the audit sink writes
    // directly to the DB.
    let audit_sink: Arc<dyn agentjail_phantom::AuditSink> = match pg.as_ref() {
        Some(p) => Arc::new(AuditStoreSink::new(Arc::new(PgAuditStore::new(p.pool.clone())))),
        None    => Arc::new(AuditStoreSink::new(stores.audit.clone())),
    };
    let proxy = PhantomProxy::builder()
        .provider(Arc::new(OpenAiProvider::new()))?
        .provider(Arc::new(AnthropicProvider::new()))?
        .provider(Arc::new(GitHubProvider::new()))?
        .provider(Arc::new(StripeProvider::new()))?
        .tokens(stores.tokens.clone())
        .keys(stores.keys.clone())
        .audit(audit_sink)
        .build()?;

    // Spawn the proxy.
    let proxy_shutdown = wait_for(shutdown_rx.clone());
    let proxy_handle = tokio::spawn({
        let p = proxy.clone();
        let addr = config.proxy_addr;
        async move {
            tracing::info!(%addr, "phantom proxy listening");
            p.serve(addr, proxy_shutdown).await
        }
    });

    // Build every durable store up front so background sweepers (snapshot
    // GC, idle reaper, gateway) share the *same* Arc instances as the
    // control plane. In in-memory mode this keeps a single source of
    // truth across the router + reapers; in PG mode it just means fewer
    // pool references to the same backend.
    let sessions: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::new());
    let (credentials, audit_store, jails_store, workspace_store, snapshot_store) =
        match pg.as_ref() {
            Some(p) => (
                Arc::new(PgCredentialStore::new(p.pool.clone())) as Arc<dyn CredentialStore>,
                Arc::new(PgAuditStore::new(p.pool.clone())) as Arc<dyn AuditStore>,
                Arc::new(PgJailStore::new(p.pool.clone())) as Arc<dyn JailStore>,
                Arc::new(PgWorkspaceStore::new(p.pool.clone())) as Arc<dyn WorkspaceStore>,
                Arc::new(PgSnapshotStore::new(p.pool.clone())) as Arc<dyn SnapshotStore>,
            ),
            None => (
                Arc::new(InMemoryCredentialStore::new()) as Arc<dyn CredentialStore>,
                stores.audit.clone() as Arc<dyn AuditStore>,
                Arc::new(InMemoryJailStore::new()) as Arc<dyn JailStore>,
                Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>,
                Arc::new(InMemorySnapshotStore::new()) as Arc<dyn SnapshotStore>,
            ),
        };

    // Jail-IP registry is shared between the ctl (writes on exec)
    // and the gateway (reads on request) so the vm_port domain form
    // can resolve to `http://<live_ip>:<port>/` without a new
    // endpoint. One Arc, two readers.
    let gateway_jail_ips = Arc::new(agentjail_ctl::ActiveJailIps::new());

    // Build the control plane with the shared store Arcs.
    let cfg = ControlPlaneConfig {
        tokens: stores.tokens.clone(),
        keys: stores.keys.clone(),
        proxy_base_url: config.proxy_base_url.clone(),
        api_keys: config.api_keys.clone(),
        exec: Some(agentjail_ctl::ExecConfig::default()),
        state_dir: config.state_dir.clone(),
        snapshot_pool_dir: config.snapshot_pool_dir.clone(),
        platform: Some(agentjail_ctl::PlatformInfo {
            providers: registered_providers(),
            ctl_addr: Some(config.ctl_addr),
            proxy_addr: Some(config.proxy_addr),
            gateway_addr: config.gateway_addr,
            snapshot_gc: Some(config.snapshot_gc.clone()),
            idle_check_interval_secs: config.idle_check_interval_secs,
        }),
        active_jail_ips: Some(gateway_jail_ips.clone()),
    };
    let ctl = ControlPlane::with_all_stores(
        cfg,
        sessions,
        credentials,
        audit_store,
        jails_store,
        workspace_store.clone(),
        snapshot_store.clone(),
    );
    // Drop rows whose on-disk dirs have disappeared (e.g. tmpfs wipe).
    ctl.reconcile().await;

    let _gc_task = snapshot_gc::spawn_sweeper(snapshot_store.clone(), config.snapshot_gc);

    let _idle_task = spawn_idle_sweeper(IdleReaperConfig {
        workspaces: workspace_store.clone(),
        snapshots:  snapshot_store,
        state_dir:  config
            .state_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("agentjail-state")),
        pool_dir:   config.snapshot_pool_dir.clone(),
        tick_secs:  config.idle_check_interval_secs,
    });

    // Hostname-routed reverse proxy (opt-in via AGENTJAIL_GATEWAY_ADDR).
    let gateway_handle = if let Some(addr) = config.gateway_addr {
        let gw_rx = shutdown_rx.clone();
        let state = GatewayState::new(workspace_store.clone(), gateway_jail_ips.clone())
            .context("build gateway HTTP client")?;
        Some(tokio::spawn(async move {
            if let Err(e) = gateway::serve(addr, state, gw_rx).await {
                tracing::error!(error = %e, "gateway listener ended");
            }
        }))
    } else {
        None
    };

    let router = ctl.router();

    let ctl_listener = TcpListener::bind(config.ctl_addr)
        .await
        .with_context(|| format!("bind control plane to {}", config.ctl_addr))?;
    tracing::info!(addr = %config.ctl_addr, "control plane listening");

    let ctl_shutdown = wait_for(shutdown_rx.clone());
    let ctl_handle = tokio::spawn(async move {
        axum::serve(ctl_listener, router)
            .with_graceful_shutdown(ctl_shutdown)
            .await
    });

    // Block on ctrl-c / SIGTERM.
    wait_for_signal().await;
    tracing::info!("shutdown requested, draining in-flight requests");
    let _ = shutdown_tx.send(true);

    // Wait for every listener to drain. Ignore JoinError.
    let _ = ctl_handle.await;
    let _ = proxy_handle.await;
    if let Some(h) = gateway_handle {
        let _ = h.await;
    }
    tracing::info!("goodbye");
    Ok(())
}

async fn wait_for(mut rx: watch::Receiver<bool>) {
    let _ = rx.changed().await;
}

/// Providers exposed via `GET /v1/config` for the Settings page.
/// Mirrors the `PhantomProxy::builder().provider(...)` calls in `main`
/// — if you register a new provider there, add it here too so the UI
/// lists it.
fn registered_providers() -> Vec<agentjail_ctl::ProviderInfo> {
    use agentjail_phantom::Provider;
    let pool: Vec<Arc<dyn Provider>> = vec![
        Arc::new(OpenAiProvider::new()),
        Arc::new(AnthropicProvider::new()),
        Arc::new(GitHubProvider::new()),
        Arc::new(StripeProvider::new()),
    ];
    pool.into_iter()
        .map(|p| agentjail_ctl::ProviderInfo {
            service_id:     p.id().to_string(),
            upstream_base:  p.upstream_base().to_string(),
            request_prefix: format!("/v1/{}/", p.id()),
        })
        .collect()
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = signal::ctrl_c().await;
    }
}

// ---------- config ----------

struct Config {
    ctl_addr: SocketAddr,
    proxy_addr: SocketAddr,
    proxy_base_url: String,
    api_keys: Vec<String>,
    state_dir: Option<std::path::PathBuf>,
    snapshot_pool_dir: Option<std::path::PathBuf>,
    snapshot_gc: SnapshotGcConfig,
    idle_check_interval_secs: u64,
    gateway_addr: Option<SocketAddr>,
}

impl Config {
    fn from_env() -> Result<Self> {
        let ctl_addr: SocketAddr = std::env::var("CTL_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:7000".into())
            .parse()
            .context("parsing CTL_ADDR")?;
        let proxy_addr: SocketAddr = std::env::var("PROXY_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8443".into())
            .parse()
            .context("parsing PROXY_ADDR")?;
        let proxy_base_url =
            std::env::var("PROXY_BASE_URL").unwrap_or_else(|_| format!("http://{proxy_addr}"));
        let api_keys = std::env::var("AGENTJAIL_API_KEY")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();

        if api_keys.is_empty() {
            tracing::warn!(
                "AGENTJAIL_API_KEY unset — control plane is OPEN. \
                 Set it in any non-dev environment."
            );
        }

        let state_dir = std::env::var("AGENTJAIL_STATE_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);

        let snapshot_pool_dir = std::env::var("AGENTJAIL_SNAPSHOT_POOL_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);
        if snapshot_pool_dir.is_some() {
            tracing::info!("content-addressed snapshot pool enabled");
        }

        let snapshot_gc = SnapshotGcConfig {
            max_age_secs: parse_env_u64("AGENTJAIL_SNAPSHOT_MAX_AGE_SECS"),
            max_count:    parse_env_u64("AGENTJAIL_SNAPSHOT_MAX_COUNT")
                            .map(|v| v as usize),
            tick_secs:    parse_env_u64("AGENTJAIL_SNAPSHOT_GC_TICK_SECS").unwrap_or(60),
        };
        if snapshot_gc.is_enabled() {
            tracing::info!(
                max_age_secs = ?snapshot_gc.max_age_secs,
                max_count    = ?snapshot_gc.max_count,
                tick_secs    = snapshot_gc.tick_secs,
                "snapshot gc sweeper enabled"
            );
        }

        let idle_check_interval_secs =
            parse_env_u64("AGENTJAIL_IDLE_CHECK_INTERVAL_SECS").unwrap_or(30);

        let gateway_addr = std::env::var("AGENTJAIL_GATEWAY_ADDR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .and_then(|s| s.parse::<SocketAddr>().ok());

        Ok(Self {
            ctl_addr,
            proxy_addr,
            proxy_base_url,
            api_keys,
            state_dir,
            snapshot_pool_dir,
            snapshot_gc,
            idle_check_interval_secs,
            gateway_addr,
        })
    }
}

/// Install the tracing subscriber.
///
/// Format is controlled by `LOG_FORMAT`:
///   - `json` — one JSON object per line; fields are key-value.
///     Ideal for prod + log aggregators.
///   - anything else (or unset) — compact single-line text. Default.
///
/// Log level follows `RUST_LOG`; defaults to `info` when unset.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    if std::env::var("LOG_FORMAT").ok().as_deref() == Some("json") {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .with_target(true)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .compact()
            .init();
    }
}

fn parse_env_u64(var: &str) -> Option<u64> {
    std::env::var(var)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

struct Stores {
    /// Polymorphic: either the in-memory default (dev / no-DB mode)
    /// or [`PgTokenStore`] wrapped in [`LruTokenCache`] when a
    /// database is configured. The proxy's hot-path `lookup` only
    /// ever sees `dyn TokenStore`, so both shapes plug in identically.
    tokens: Arc<dyn TokenStore>,
    keys: Arc<InMemoryKeyStore>,
    audit: Arc<InMemoryAuditStore>,
}

impl Stores {
    fn new_from_env() -> Self {
        let keys = Arc::new(InMemoryKeyStore::new());
        seed_if_set(&keys, ServiceId::OpenAi, "OPENAI_API_KEY");
        seed_if_set(&keys, ServiceId::Anthropic, "ANTHROPIC_API_KEY");
        seed_if_set(&keys, ServiceId::GitHub, "GITHUB_TOKEN");
        seed_if_set(&keys, ServiceId::Stripe, "STRIPE_API_KEY");
        Self {
            // Default is in-memory; `attach_postgres_tokens` swaps it
            // for a DB-backed + cached store once the pool is ready.
            tokens: Arc::new(InMemoryTokenStore::new()),
            keys,
            audit: Arc::new(InMemoryAuditStore::new()),
        }
    }
}

/// Token-cache capacity. 10k entries ≈ ~1 MB — plenty for a single
/// agentjail-server handling up to a few thousand concurrent
/// sessions. Tune via `AGENTJAIL_TOKEN_CACHE_SIZE` if needed.
fn token_cache_capacity() -> usize {
    std::env::var("AGENTJAIL_TOKEN_CACHE_SIZE")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(10_000)
}

fn seed_if_set(keys: &InMemoryKeyStore, service: ServiceId, env_var: &str) {
    if let Ok(v) = std::env::var(env_var)
        && !v.trim().is_empty()
    {
        // Env-seeded keys land under the `"dev"` tenant — the same
        // sentinel `KeyStore::from_env()` uses and the control plane
        // emits when auth is disabled. Per-tenant keys go through
        // `POST /v1/credentials?tenant=<id>` at runtime.
        keys.set("dev", service, SecretString::new(v));
        tracing::info!(%service, %env_var, "seeded upstream key from env (tenant=dev)");
    }
}
