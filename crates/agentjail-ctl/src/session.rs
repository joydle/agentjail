//! Session storage + domain types.

use std::collections::HashMap;
use std::sync::RwLock;

use agentjail_phantom::ServiceId;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A session is one issued-to-a-sandbox bundle of phantom credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Opaque identifier, `sess_<hex>`.
    pub id: String,
    /// Tenant that owns this session. Stamped from the caller's
    /// `TenantScope` on create; cross-tenant reads are filtered out.
    pub tenant_id: String,
    /// When the session was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the session's tokens expire. `None` = no expiry.
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// Services the session has phantom tokens for.
    pub services: Vec<ServiceId>,
    /// The environment variable map handed to the sandbox.
    /// Keys: `OPENAI_API_KEY`, `OPENAI_BASE_URL`, ...
    pub env: HashMap<String, String>,
}

/// Storage for sessions. Safe for concurrent use.
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Persist a new session. Returns an error if the id is already used.
    async fn insert(&self, session: Session) -> crate::Result<()>;
    /// Fetch by id.
    async fn get(&self, id: &str) -> Option<Session>;
    /// Return every session, ordered newest first. `tenant`:
    /// `Some(id)` restricts to a single tenant (operator); `None`
    /// returns every tenant's sessions (admin).
    async fn list(&self, tenant: Option<&str>) -> Vec<Session>;
    /// Remove and return the session.
    async fn remove(&self, id: &str) -> Option<Session>;
}

/// In-memory implementation. Lost on restart.
#[derive(Default)]
pub struct InMemorySessionStore {
    inner: RwLock<HashMap<String, Session>>,
}

impl InMemorySessionStore {
    /// New, empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl SessionStore for InMemorySessionStore {
    async fn insert(&self, session: Session) -> crate::Result<()> {
        let mut g = self
            .inner
            .write()
            .map_err(|_| crate::CtlError::Internal("session store poisoned".into()))?;
        if g.contains_key(&session.id) {
            return Err(crate::CtlError::Conflict(format!(
                "session {} already exists",
                session.id
            )));
        }
        g.insert(session.id.clone(), session);
        Ok(())
    }

    async fn get(&self, id: &str) -> Option<Session> {
        self.inner.read().ok()?.get(id).cloned()
    }

    async fn list(&self, tenant: Option<&str>) -> Vec<Session> {
        let Ok(g) = self.inner.read() else {
            return Vec::new();
        };
        let mut v: Vec<Session> = g
            .values()
            .filter(|s| match tenant {
                None    => true,
                Some(t) => s.tenant_id == t,
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v
    }

    async fn remove(&self, id: &str) -> Option<Session> {
        self.inner.write().ok()?.remove(id)
    }
}

/// Random 12-byte session id rendered as `sess_<24hex>`.
#[must_use]
pub(crate) fn new_session_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    format!("sess_{}", hex::encode(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample(id: &str, tenant: &str) -> Session {
        Session {
            id: id.into(),
            tenant_id: tenant.into(),
            created_at: OffsetDateTime::now_utc(),
            expires_at: None,
            services: Vec::new(),
            env: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn list_filters_by_tenant_when_some() {
        let store = InMemorySessionStore::new();
        store.insert(sample("sess_a", "acme")).await.unwrap();
        store.insert(sample("sess_b", "other")).await.unwrap();
        store.insert(sample("sess_c", "acme")).await.unwrap();

        let rows = store.list(Some("acme")).await;
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r.tenant_id, "acme");
        }
    }

    #[tokio::test]
    async fn list_none_returns_every_tenant() {
        let store = InMemorySessionStore::new();
        store.insert(sample("sess_a", "acme")).await.unwrap();
        store.insert(sample("sess_b", "other")).await.unwrap();

        assert_eq!(store.list(None).await.len(), 2);
    }
}
