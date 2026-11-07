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
//! | `AGENTJAIL_API_KEY`| — (required)         | Comma-separated `token@tenant:role` keys. Startup fails without a valid one. |
//! | `AGENTJAIL_DEV_INSECURE` | — | `1` = allow starting with no API key (every caller is admin). Dev only. |
//! | `AGENTJAIL_USER_NAMESPACE` | `1` | `0` disables user namespaces for jails |
//! | `AGENTJAIL_LANDLOCK` | `1` | `0` disables landlock for jails |
//! | `AGENTJAIL_MIN_MEMORY_MB` / `AGENTJAIL_MAX_MEMORY_MB` | 64 / 8192 | memory_mb clamp |
//! | `AGENTJAIL_MAX_CONCURRENT` | 16 | Global concurrent jails |
//! | `AGENTJAIL_MAX_CONCURRENT_PER_TENANT` | 0 (off) | Per-tenant concurrent jails |
//! | `AGENTJAIL_TOKEN_MAX_TTL_SECS` | — | Upper bound on phantom-token lifetime |
//! | `AGENTJAIL_ALLOW_PRIVATE_BACKENDS` | — | `1` = let workspace backend_urls reach private/loopback addresses (dev only) |
//! | `AGENTJAIL_STATE_DIR` | `$TMPDIR/agentjail-state` | Persistent workspace + snapshot root |
//! | `AGENTJAIL_SNAPSHOT_MAX_AGE_SECS` | — | Drop snapshots older than N sec (GC) |
//! | `AGENTJAIL_SNAPSHOT_MAX_COUNT`    | — | Keep at most N snapshots (GC)        |
//! | `AGENTJAIL_SNAPSHOT_GC_TICK_SECS` | 60 | GC sweep interval                   |
//! | `AGENTJAIL_SNAPSHOT_POOL_DIR`     | — | Content-addressed snapshot pool      |
//! | `AGENTJAIL_IDLE_CHECK_INTERVAL_SECS` | 30 | Workspace idle-reaper interval   |
//! | `AGENTJAIL_GATEWAY_ADDR`          | — | Hostname-routed proxy bind (e.g. 0.0.0.0:8080) |
//! | `AGENTJAIL_GATEWAY_MAX_INFLIGHT`  | 64 | Gateway requests handled at once (503 beyond) |
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
    ApiKeys, AuditStore, AuditStoreSink, ControlPlane, ControlPlaneConfig, CredentialStore,
    InMemoryAuditStore, InMemoryCredentialStore, InMemoryJailStore, InMemorySessionStore,
    InMemorySnapshotStore, InMemoryWorkspaceStore, JailStore, PgAuditStore, PgCredentialStore,
    PgJailStore, PgSnapshotStore, PgWorkspaceStore, Postgres, SessionStore, SnapshotGcConfig,
    SnapshotStore, WorkspaceStore, snapshot_gc,
    workspace_idle::{IdleReaperConfig, spawn_sweeper as spawn_idle_sweeper},
};

mod gateway;
use agentjail_phantom::providers::{
    AnthropicProvider, GitHubProvider, OpenAiProvider, StripeProvider,
};
use agentjail_phantom::{
    InMemoryKeyStore, InMemoryTokenStore, LruTokenCache, MaxTtlTokenStore, PhantomProxy,
    SecretString, ServiceId, TokenStore,
};
use anyhow::{Context, Result};
use gateway::GatewayState;
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
    let pg = match std::env::var("DATABASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        Some(url) => {
            tracing::info!("connecting to postgres");
            // Never put the DSN itself in an error / log line: it carries
            // the database password.
            let pg = Postgres::connect(&url)
                .await
                .with_context(|| format!("connecting to postgres at {}", redact_dsn(&url)))?;
            let rehydrated = pg.rehydrate_keys(&stores.keys).await?;
            tracing::info!(%rehydrated, "postgres ready");

            // Durable tokens: DB is source of truth, LRU cache fronts
            // the proxy's hot-path lookup. Cache size configurable via
            // `AGENTJAIL_TOKEN_CACHE_SIZE`.
            let raw = agentjail_ctl::PgTokenStore::new(pg.pool.clone());
            let cap = token_cache_capacity();
            stores.tokens = match config.token_max_ttl {
                Some(max) => Arc::new(MaxTtlTokenStore::new(LruTokenCache::new(raw, cap), max))
                    as Arc<dyn TokenStore>,
                None => Arc::new(LruTokenCache::new(raw, cap)) as Arc<dyn TokenStore>,
            };
            tracing::info!(capacity = cap, "phantom token store: postgres + LRU cache");
            Some(pg)
        }
        None => {
            tracing::warn!(
                "DATABASE_URL unset — using in-memory stores (state is lost on restart)"
            );
            if let Some(max) = config.token_max_ttl {
                stores.tokens = Arc::new(MaxTtlTokenStore::new(InMemoryTokenStore::new(), max));
            }
            None
        }
    };

    // Cross-service shutdown signal.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Build the phantom proxy. When Postgres is on, the proxy still reads
    // keys from the in-memory store (hot path) but the audit sink writes
    // directly to the DB.
    let audit_sink: Arc<dyn agentjail_phantom::AuditSink> = match pg.as_ref() {
        Some(p) => Arc::new(AuditStoreSink::new(Arc::new(PgAuditStore::new(
            p.pool.clone(),
        )))),
        None => Arc::new(AuditStoreSink::new(stores.audit.clone())),
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
    let (credentials, audit_store, jails_store, workspace_store, snapshot_store) = match pg.as_ref()
    {
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
        exec: Some(config.exec.clone()),
        state_dir: config.state_dir.clone(),
        snapshot_pool_dir: config.snapshot_pool_dir.clone(),
        platform: Some(agentjail_ctl::PlatformInfo {
            providers: registered_providers(),
            ctl_addr: Some(config.ctl_addr),
            proxy_addr: Some(config.proxy_addr),
            gateway_addr: config.gateway_addr,
            snapshot_gc: Some(config.snapshot_gc),
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
    )
    .allow_private_backends(config.allow_private_backends);
    // Drop rows whose on-disk dirs have disappeared (e.g. tmpfs wipe).
    ctl.reconcile().await;

    let _gc_task = snapshot_gc::spawn_sweeper(snapshot_store.clone(), config.snapshot_gc);

    let _idle_task = spawn_idle_sweeper(IdleReaperConfig {
        workspaces: workspace_store.clone(),
        snapshots: snapshot_store,
        state_dir: config
            .state_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("agentjail-state")),
        pool_dir: config.snapshot_pool_dir.clone(),
        tick_secs: config.idle_check_interval_secs,
    });

    // Hostname-routed reverse proxy (opt-in via AGENTJAIL_GATEWAY_ADDR).
    let gateway_handle = if let Some(addr) = config.gateway_addr {
        let gw_rx = shutdown_rx.clone();
        let mut state = GatewayState::with_policy(
            workspace_store.clone(),
            gateway_jail_ips.clone(),
            config.allow_private_backends,
        )
        .context("build gateway HTTP client")?;
        if let Some(n) = config.gateway_max_inflight {
            state = state.max_inflight(n);
        }
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
            service_id: p.id().to_string(),
            upstream_base: p.upstream_base().to_string(),
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
    gateway_max_inflight: Option<usize>,
    exec: agentjail_ctl::ExecConfig,
    token_max_ttl: Option<std::time::Duration>,
    allow_private_backends: bool,
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

        check_auth_config(&api_keys, env_flag("AGENTJAIL_DEV_INSECURE"))?;

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
            max_count: parse_env_u64("AGENTJAIL_SNAPSHOT_MAX_COUNT").map(|v| v as usize),
            tick_secs: parse_env_u64("AGENTJAIL_SNAPSHOT_GC_TICK_SECS").unwrap_or(60),
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
        let gateway_max_inflight = parse_env_u64("AGENTJAIL_GATEWAY_MAX_INFLIGHT")
            .filter(|&n| n > 0)
            .map(|n| n as usize);

        let exec = exec_config_from_env();
        if !exec.user_namespace {
            tracing::warn!("AGENTJAIL_USER_NAMESPACE=0 — jails run without a user namespace");
        }
        if !exec.landlock {
            tracing::warn!("AGENTJAIL_LANDLOCK=0 — jails run without landlock");
        }
        let token_max_ttl = parse_env_u64("AGENTJAIL_TOKEN_MAX_TTL_SECS")
            .filter(|&n| n > 0)
            .map(std::time::Duration::from_secs);
        let allow_private_backends = env_flag("AGENTJAIL_ALLOW_PRIVATE_BACKENDS");
        if allow_private_backends {
            tracing::warn!(
                "AGENTJAIL_ALLOW_PRIVATE_BACKENDS=1 — workspace backend_urls may reach \
                 loopback/private/metadata addresses (SSRF guard off)"
            );
        }

        Ok(Self {
            ctl_addr,
            proxy_addr,
            proxy_base_url,
            api_keys,
            state_dir,
            snapshot_pool_dir,
            snapshot_gc,
            idle_check_interval_secs,
            gateway_max_inflight,
            gateway_addr,
            exec,
            token_max_ttl,
            allow_private_backends,
        })
    }
}

/// Fail-closed auth policy, shared by every startup path:
///
/// - keys configured but none parse → refuse (a typo must not open the
///   control plane);
/// - no keys at all → refuse, unless the operator explicitly opted into
///   `AGENTJAIL_DEV_INSECURE=1`, in which case every caller is admin.
fn check_auth_config(api_keys: &[String], dev_insecure: bool) -> Result<()> {
    if api_keys.is_empty() {
        if !dev_insecure {
            anyhow::bail!(
                "AGENTJAIL_API_KEY is not set; refusing to start with the control plane \
                 open. Set AGENTJAIL_API_KEY=<token>@<tenant>:<role>, or \
                 AGENTJAIL_DEV_INSECURE=1 for local development only"
            );
        }
        tracing::warn!(
            "AGENTJAIL_DEV_INSECURE=1 and no AGENTJAIL_API_KEY — control plane is OPEN \
             and every caller is admin. Never use this outside local development."
        );
        return Ok(());
    }
    if !ApiKeys::from_config_strings(api_keys.to_vec()).is_enforced() {
        // Fail closed: a key was configured but none parsed, which
        // would otherwise silently disable auth.
        anyhow::bail!(
            "AGENTJAIL_API_KEY is set but no entry is a valid \
             `token@tenant:role`; refusing to start with auth disabled"
        );
    }
    Ok(())
}

/// `1` / `true` / `yes` / `on` (case-insensitive).
fn env_flag(var: &str) -> bool {
    std::env::var(var)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Boolean env with a default; anything other than an explicit
/// false-ish value keeps the default.
fn env_bool(var: &str, default: bool) -> bool {
    match std::env::var(var) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "no" | "off" => false,
            "1" | "true" | "yes" | "on" => true,
            _ => default,
        },
        Err(_) => default,
    }
}

fn exec_config_from_env() -> agentjail_ctl::ExecConfig {
    let d = agentjail_ctl::ExecConfig::default();
    agentjail_ctl::ExecConfig {
        max_concurrent: parse_env_u64("AGENTJAIL_MAX_CONCURRENT")
            .filter(|&n| n > 0)
            .map_or(d.max_concurrent, |n| n as usize),
        max_concurrent_per_tenant: parse_env_u64("AGENTJAIL_MAX_CONCURRENT_PER_TENANT")
            .map_or(d.max_concurrent_per_tenant, |n| n as usize),
        min_memory_mb: parse_env_u64("AGENTJAIL_MIN_MEMORY_MB")
            .filter(|&n| n > 0)
            .unwrap_or(d.min_memory_mb),
        max_memory_mb: parse_env_u64("AGENTJAIL_MAX_MEMORY_MB")
            .filter(|&n| n > 0)
            .unwrap_or(d.max_memory_mb),
        user_namespace: env_bool("AGENTJAIL_USER_NAMESPACE", d.user_namespace),
        landlock: env_bool("AGENTJAIL_LANDLOCK", d.landlock),
        ..d
    }
}

/// `postgres://user:secret@host/db` → `postgres://user:***@host/db`.
/// Unparseable input is replaced wholesale so nothing leaks.
fn redact_dsn(dsn: &str) -> String {
    match reqwest::Url::parse(dsn) {
        Ok(mut u) => {
            if u.password().is_some() {
                let _ = u.set_password(Some("***"));
            }
            // libpq also accepts `?password=...`.
            if u.query_pairs()
                .any(|(k, _)| k.eq_ignore_ascii_case("password"))
            {
                let pairs: Vec<(String, String)> = u
                    .query_pairs()
                    .map(|(k, v)| {
                        if k.eq_ignore_ascii_case("password") {
                            (k.into_owned(), "***".to_string())
                        } else {
                            (k.into_owned(), v.into_owned())
                        }
                    })
                    .collect();
                u.query_pairs_mut().clear().extend_pairs(pairs);
            }
            u.to_string()
        }
        Err(_) => "<unparseable DATABASE_URL>".to_string(),
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

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_api_key_refuses_to_start() {
        let err = check_auth_config(&[], false).unwrap_err().to_string();
        assert!(err.contains("AGENTJAIL_API_KEY"), "{err}");
    }

    #[test]
    fn missing_api_key_allowed_only_with_dev_insecure() {
        assert!(check_auth_config(&[], true).is_ok());
    }

    #[test]
    fn malformed_api_key_refuses_even_with_dev_insecure() {
        // A configured-but-broken key must never degrade to "open".
        assert!(check_auth_config(&["no spaces allowed".into()], false).is_err());
        assert!(check_auth_config(&["no spaces allowed".into()], true).is_err());
    }

    #[test]
    fn valid_api_key_starts() {
        assert!(check_auth_config(&["aj_x@acme:admin".into()], false).is_ok());
    }

    #[test]
    fn dsn_password_is_redacted() {
        let r = redact_dsn("postgres://agentjail:s3cr3t@postgres:5432/agentjail");
        assert!(!r.contains("s3cr3t"), "{r}");
        assert!(r.contains("agentjail:***@postgres"), "{r}");
        let r = redact_dsn("postgres://u@h/db?sslmode=require&password=hunter2");
        assert!(!r.contains("hunter2"), "{r}");
        let r = redact_dsn("not a url with password=hunter2");
        assert!(!r.contains("hunter2"), "{r}");
    }

    #[test]
    fn exec_hardening_defaults_on() {
        let c = exec_config_from_env();
        assert!(c.user_namespace || std::env::var("AGENTJAIL_USER_NAMESPACE").is_ok());
        assert!(c.landlock || std::env::var("AGENTJAIL_LANDLOCK").is_ok());
        assert!(c.min_memory_mb >= 1);
    }
}
