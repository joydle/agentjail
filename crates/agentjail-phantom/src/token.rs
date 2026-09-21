//! Phantom tokens and token storage.
//!
//! Tokens are 256 bits of CSPRNG output rendered as `phm_<64-hex>`. They
//! carry no meaning by themselves — the store resolves them to a session,
//! service, and scope list.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

use rand::RngCore;
use subtle::ConstantTimeEq;

use crate::provider::ServiceId;

/// `phm_` + 64 lowercase hex characters.
pub const TOKEN_PREFIX: &str = "phm_";
/// Length of the hex body (32 bytes = 64 hex chars).
pub const TOKEN_HEX_LEN: usize = 64;

/// An opaque phantom token. Compare with `ct_eq`, never `==`.
#[derive(Clone)]
pub struct PhantomToken {
    bytes: [u8; 32],
}

impl PhantomToken {
    /// Mint a fresh token from the OS CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Parse a `phm_<hex>` string. Returns `None` for anything malformed.
    /// Tolerant of surrounding whitespace and an optional `Bearer ` prefix.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        let s = s.strip_prefix("Bearer ").unwrap_or(s).trim();
        let hex_part = s.strip_prefix(TOKEN_PREFIX)?;
        if hex_part.len() != TOKEN_HEX_LEN {
            return None;
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(hex_part, &mut bytes).ok()?;
        Some(Self { bytes })
    }

    /// Constant-time equality.
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        self.bytes.ct_eq(&other.bytes).into()
    }

    /// Raw bytes. Avoid — prefer `ct_eq`. Exposed for storage-key hashing.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl std::fmt::Debug for PhantomToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the full token in logs.
        write!(f, "phm_<redacted>")
    }
}

impl std::fmt::Display for PhantomToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(TOKEN_PREFIX)?;
        for b in self.bytes {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// A single path glob, matched against the request path *after* the service
/// prefix has been stripped. Supports a trailing `*` wildcard.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PathGlob {
    pattern: String,
}

impl PathGlob {
    /// Create a glob from a string such as `/v1/chat/completions` or
    /// `/v1/chat/*`.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
        }
    }

    /// Check whether `path` is allowed by this glob.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        if let Some(prefix) = self.pattern.strip_suffix('*') {
            path.starts_with(prefix)
        } else {
            path == self.pattern
        }
    }
}

/// A scope limits what a token can do with its service.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Scope {
    /// Allowed paths. Empty list = allow any path.
    pub allowed_paths: Vec<PathGlob>,
}

impl Scope {
    /// Unrestricted scope — any path is allowed.
    #[must_use]
    pub fn any() -> Self {
        Self {
            allowed_paths: Vec::new(),
        }
    }

    /// Check whether `path` satisfies this scope.
    #[must_use]
    pub fn allows_path(&self, path: &str) -> bool {
        self.allowed_paths.is_empty() || self.allowed_paths.iter().any(|g| g.matches(path))
    }
}

/// A token's resolved metadata.
#[derive(Debug, Clone)]
pub struct TokenRecord {
    /// Session this token belongs to. Opaque to the proxy.
    pub session_id: String,
    /// Tenant the issuing session belongs to. The proxy looks up the
    /// real upstream key using this + [`Self::service`], so a token
    /// issued to tenant A can only spend tenant A's credential.
    pub tenant_id: String,
    /// Which upstream this token can reach.
    pub service: ServiceId,
    /// What paths within that upstream.
    pub scope: Scope,
    /// Optional wall-clock expiry.
    pub expires_at: Option<SystemTime>,
}

impl TokenRecord {
    fn is_expired(&self, now: SystemTime) -> bool {
        self.expires_at.is_some_and(|e| now >= e)
    }
}

/// Storage for live tokens. Implementations must be safe for concurrent use.
#[async_trait::async_trait]
pub trait TokenStore: Send + Sync + 'static {
    /// Mint a token for this session/service/scope and persist it.
    async fn issue(
        &self,
        session_id: String,
        tenant_id: String,
        service: ServiceId,
        scope: Scope,
        ttl: Option<Duration>,
    ) -> PhantomToken;

    /// Resolve a token to its record, or `None` if unknown or expired.
    async fn lookup(&self, token: &PhantomToken) -> Option<TokenRecord>;

    /// Revoke a single token. Idempotent.
    async fn revoke(&self, token: &PhantomToken);

    /// Revoke every token for a session. Idempotent.
    async fn revoke_session(&self, session_id: &str);
}

/// In-memory token store. Lost on restart — use this for dev and tests.
#[derive(Default)]
pub struct InMemoryTokenStore {
    /// Keyed by the token's raw bytes. The bytes themselves are the lookup key;
    /// this is fine because the token is a uniform-random 256-bit value, so
    /// hashmap probing gives no side-channel on other tokens.
    inner: RwLock<HashMap<[u8; 32], TokenRecord>>,
}

impl InMemoryTokenStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl TokenStore for InMemoryTokenStore {
    async fn issue(
        &self,
        session_id: String,
        tenant_id: String,
        service: ServiceId,
        scope: Scope,
        ttl: Option<Duration>,
    ) -> PhantomToken {
        let token = PhantomToken::generate();
        let record = TokenRecord {
            session_id,
            tenant_id,
            service,
            scope,
            expires_at: ttl.map(|d| SystemTime::now() + d),
        };
        if let Ok(mut g) = self.inner.write() {
            g.insert(*token.as_bytes(), record);
        }
        token
    }

    async fn lookup(&self, token: &PhantomToken) -> Option<TokenRecord> {
        let g = self.inner.read().ok()?;
        let rec = g.get(token.as_bytes())?.clone();
        if rec.is_expired(SystemTime::now()) {
            return None;
        }
        Some(rec)
    }

    async fn revoke(&self, token: &PhantomToken) {
        if let Ok(mut g) = self.inner.write() {
            g.remove(token.as_bytes());
        }
    }

    async fn revoke_session(&self, session_id: &str) {
        if let Ok(mut g) = self.inner.write() {
            g.retain(|_, rec| rec.session_id != session_id);
        }
    }
}

// ---------- LRU cache wrapper ----------

/// Wrap any [`TokenStore`] in a bounded in-memory LRU cache so the
/// phantom proxy's hot-path `lookup` doesn't hit Postgres on every
/// upstream call.
///
/// The cache is read-through (miss → delegate → insert) and
/// write-through (`issue` / `revoke*` go to both). `revoke_session`
/// invalidates by linear scan; sessions are low cardinality (thousands,
/// not millions), and session-wide revocation is rare — so the
/// quadratic-vs-N behaviour is fine.
///
/// Eviction is FIFO on capacity. Not strict LRU, but token access is
/// uniform-random — strict recency doesn't buy much, and keeping the
/// cache lockless-friendly matters more on the hot path.
pub struct LruTokenCache<Inner: TokenStore> {
    inner: Inner,
    cache: tokio::sync::RwLock<LruInner>,
    capacity: usize,
}

struct LruInner {
    map: HashMap<[u8; 32], TokenRecord>,
    order: std::collections::VecDeque<[u8; 32]>,
}

impl LruInner {
    fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::with_capacity(capacity),
            order: std::collections::VecDeque::with_capacity(capacity),
        }
    }

    fn put(&mut self, key: [u8; 32], rec: TokenRecord, capacity: usize) {
        if self.map.insert(key, rec).is_none() {
            self.order.push_back(key);
        }
        while self.map.len() > capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            } else {
                break;
            }
        }
    }

    fn remove(&mut self, key: &[u8; 32]) {
        self.map.remove(key);
        // Leave the entry in `order` — it'll be a cheap no-op eviction
        // later. Cleaning here is O(n) and wasteful given how rare
        // individual revokes are.
    }

    fn clear_session(&mut self, session_id: &str) {
        self.map.retain(|_, rec| rec.session_id != session_id);
    }
}

impl<Inner: TokenStore> LruTokenCache<Inner> {
    /// Wrap `inner` with an LRU cache capped at `capacity` entries.
    /// A cap of 10_000 — ~1 MB memory assuming ~100 B per record — is
    /// a sensible default for a single-tenant deployment; scale up
    /// linearly with expected concurrent sessions.
    #[must_use]
    pub fn new(inner: Inner, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            inner,
            cache: tokio::sync::RwLock::new(LruInner::new(capacity)),
            capacity,
        }
    }
}

#[async_trait::async_trait]
impl<Inner: TokenStore> TokenStore for LruTokenCache<Inner> {
    async fn issue(
        &self,
        session_id: String,
        tenant_id: String,
        service: ServiceId,
        scope: Scope,
        ttl: Option<Duration>,
    ) -> PhantomToken {
        let token = self.inner.issue(
            session_id.clone(), tenant_id.clone(), service, scope.clone(), ttl,
        ).await;
        // Pull the canonical record back through `lookup` so the
        // cached entry exactly matches what the store would return
        // (particularly the resolved `expires_at`).
        if let Some(rec) = self.inner.lookup(&token).await {
            let mut g = self.cache.write().await;
            g.put(*token.as_bytes(), rec, self.capacity);
        }
        token
    }

    async fn lookup(&self, token: &PhantomToken) -> Option<TokenRecord> {
        // Fast path: cache hit.
        {
            let g = self.cache.read().await;
            if let Some(rec) = g.map.get(token.as_bytes()) {
                return Some(rec.clone());
            }
        }
        // Miss: delegate + populate.
        let rec = self.inner.lookup(token).await?;
        {
            let mut g = self.cache.write().await;
            g.put(*token.as_bytes(), rec.clone(), self.capacity);
        }
        Some(rec)
    }

    async fn revoke(&self, token: &PhantomToken) {
        self.inner.revoke(token).await;
        let mut g = self.cache.write().await;
        g.remove(token.as_bytes());
    }

    async fn revoke_session(&self, session_id: &str) {
        self.inner.revoke_session(session_id).await;
        let mut g = self.cache.write().await;
        g.clear_session(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_roundtrip() {
        let t = PhantomToken::generate();
        let s = t.to_string();
        assert!(s.starts_with(TOKEN_PREFIX));
        assert_eq!(s.len(), TOKEN_PREFIX.len() + TOKEN_HEX_LEN);
        let parsed = PhantomToken::parse(&s).unwrap();
        assert!(t.ct_eq(&parsed));
    }

    #[test]
    fn token_parse_tolerates_bearer_and_whitespace() {
        let t = PhantomToken::generate();
        let s = t.to_string();
        let parsed = PhantomToken::parse(&format!("  Bearer {s}  ")).unwrap();
        assert!(t.ct_eq(&parsed));
    }

    #[test]
    fn token_parse_rejects_garbage() {
        assert!(PhantomToken::parse("").is_none());
        assert!(PhantomToken::parse("phm_").is_none());
        assert!(PhantomToken::parse("sk-abc").is_none());
        assert!(PhantomToken::parse("phm_zz").is_none());
        // one char short
        assert!(PhantomToken::parse(&format!("phm_{}", "a".repeat(63))).is_none());
        // one char long
        assert!(PhantomToken::parse(&format!("phm_{}", "a".repeat(65))).is_none());
        // non-hex
        assert!(PhantomToken::parse(&format!("phm_{}", "z".repeat(64))).is_none());
    }

    #[test]
    fn token_debug_is_redacted() {
        let t = PhantomToken::generate();
        let debug = format!("{t:?}");
        assert_eq!(debug, "phm_<redacted>");
        assert!(!debug.contains(&t.to_string()[4..]));
    }

    #[tokio::test]
    async fn issue_and_lookup() {
        let store = InMemoryTokenStore::new();
        let tok = store
            .issue("sess_1".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
            .await;
        let rec = store.lookup(&tok).await.unwrap();
        assert_eq!(rec.session_id, "sess_1");
        assert_eq!(rec.service, ServiceId::OpenAi);
    }

    #[tokio::test]
    async fn revoke_removes_token() {
        let store = InMemoryTokenStore::new();
        let tok = store
            .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
            .await;
        assert!(store.lookup(&tok).await.is_some());
        store.revoke(&tok).await;
        assert!(store.lookup(&tok).await.is_none());
    }

    #[tokio::test]
    async fn revoke_session_clears_all_for_session() {
        let store = InMemoryTokenStore::new();
        let a = store
            .issue("s1".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
            .await;
        let b = store
            .issue("s1".into(), "dev".into(), ServiceId::Anthropic, Scope::any(), None)
            .await;
        let c = store
            .issue("s2".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
            .await;

        store.revoke_session("s1").await;

        assert!(store.lookup(&a).await.is_none());
        assert!(store.lookup(&b).await.is_none());
        assert!(store.lookup(&c).await.is_some());
    }

    #[tokio::test]
    async fn expired_tokens_do_not_resolve() {
        let store = InMemoryTokenStore::new();
        let tok = store
            .issue(
                "s".into(),
                "dev".into(),
                ServiceId::OpenAi,
                Scope::any(),
                Some(Duration::from_millis(1)),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(store.lookup(&tok).await.is_none());
    }

    #[test]
    fn path_glob_exact_and_prefix() {
        let exact = PathGlob::new("/v1/chat/completions");
        assert!(exact.matches("/v1/chat/completions"));
        assert!(!exact.matches("/v1/chat/completions/x"));
        let prefix = PathGlob::new("/v1/chat/*");
        assert!(prefix.matches("/v1/chat/completions"));
        assert!(prefix.matches("/v1/chat/x/y"));
        assert!(!prefix.matches("/v1/other"));
    }

    #[test]
    fn scope_any_allows_everything() {
        let s = Scope::any();
        assert!(s.allows_path("/anything"));
        assert!(s.allows_path("/"));
    }

    #[test]
    fn scope_enforces_allowed_paths() {
        let s = Scope {
            allowed_paths: vec![PathGlob::new("/v1/chat/*")],
        };
        assert!(s.allows_path("/v1/chat/completions"));
        assert!(!s.allows_path("/v1/files"));
    }

    // ---------- LruTokenCache ----------

    #[tokio::test]
    async fn lru_cache_reads_through_then_hits_cache() {
        let inner = InMemoryTokenStore::new();
        let tok = inner
            .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
            .await;
        let wrapped = LruTokenCache::new(inner, 4);

        // First lookup is a miss on the cache — goes to inner, then
        // populates. Second is a hit. We can't directly observe that
        // without instrumenting; instead verify both return the same
        // record (the interesting regression would be "cache returns
        // stale entry after the inner revoked").
        let a = wrapped.lookup(&tok).await.unwrap();
        assert_eq!(a.session_id, "s");
        let b = wrapped.lookup(&tok).await.unwrap();
        assert_eq!(b.session_id, "s");
    }

    #[tokio::test]
    async fn lru_cache_evicts_when_full() {
        let inner = InMemoryTokenStore::new();
        let wrapped = LruTokenCache::new(inner, 2);

        // Mint three tokens — cache holds only two. Oldest gets
        // evicted; third lookup goes back to the inner store (which
        // still has it). Correctness check: all three lookups succeed.
        let t1 = wrapped.issue("s".into(), "dev".into(), ServiceId::OpenAi,    Scope::any(), None).await;
        let t2 = wrapped.issue("s".into(), "dev".into(), ServiceId::Anthropic, Scope::any(), None).await;
        let t3 = wrapped.issue("s".into(), "dev".into(), ServiceId::GitHub,    Scope::any(), None).await;

        assert!(wrapped.lookup(&t1).await.is_some());
        assert!(wrapped.lookup(&t2).await.is_some());
        assert!(wrapped.lookup(&t3).await.is_some());
    }

    #[tokio::test]
    async fn lru_cache_invalidates_on_revoke() {
        let inner = InMemoryTokenStore::new();
        let wrapped = LruTokenCache::new(inner, 4);
        let tok = wrapped
            .issue("s".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
            .await;

        assert!(wrapped.lookup(&tok).await.is_some());
        wrapped.revoke(&tok).await;
        assert!(wrapped.lookup(&tok).await.is_none(),
            "cache must not serve revoked tokens");
    }

    #[tokio::test]
    async fn lru_cache_invalidates_on_revoke_session() {
        let inner = InMemoryTokenStore::new();
        let wrapped = LruTokenCache::new(inner, 4);
        let a = wrapped.issue("s1".into(), "dev".into(), ServiceId::OpenAi,    Scope::any(), None).await;
        let b = wrapped.issue("s1".into(), "dev".into(), ServiceId::Anthropic, Scope::any(), None).await;
        let c = wrapped.issue("s2".into(), "dev".into(), ServiceId::OpenAi,    Scope::any(), None).await;

        // Warm the cache for all three.
        assert!(wrapped.lookup(&a).await.is_some());
        assert!(wrapped.lookup(&b).await.is_some());
        assert!(wrapped.lookup(&c).await.is_some());

        wrapped.revoke_session("s1").await;

        assert!(wrapped.lookup(&a).await.is_none(), "s1/a stale in cache");
        assert!(wrapped.lookup(&b).await.is_none(), "s1/b stale in cache");
        assert!(wrapped.lookup(&c).await.is_some(), "s2 shouldn't be touched");
    }
}
