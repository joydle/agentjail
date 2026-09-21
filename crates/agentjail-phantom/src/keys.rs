//! Storage for *real* upstream keys on the host side.
//!
//! Real keys never enter the jail. They live in whatever [`KeyStore`] the
//! operator configures — env, file, keyring, Vault, etc.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::provider::ServiceId;

/// A wrapper that avoids printing secrets in logs.
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    /// Wrap a raw string as a secret.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Reveal the underlying bytes. Call sites should be audited.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Look up the real upstream key for a `(tenant, service)` pair.
///
/// Keys are scoped per tenant so one tenant's OpenAI bill never gets
/// charged by another tenant's jails. The bootstrap `"dev"` tenant
/// stands in for pre-tenancy rows + unconfigured auth — it's the
/// sentinel the control plane issues when no API key is set, and
/// nothing else.
#[async_trait]
pub trait KeyStore: Send + Sync + 'static {
    /// Return the key for `(tenant, service)`, or `None` if none is
    /// configured for that pair.
    async fn get(&self, tenant: &str, service: ServiceId) -> Option<SecretString>;
}

/// In-memory keystore. Typically populated from env or a config file at boot.
#[derive(Default)]
pub struct InMemoryKeyStore {
    inner: RwLock<HashMap<(String, ServiceId), SecretString>>,
}

impl InMemoryKeyStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or overwrite the key for a `(tenant, service)` pair.
    pub fn set(&self, tenant: impl Into<String>, service: ServiceId, key: SecretString) {
        if let Ok(mut g) = self.inner.write() {
            g.insert((tenant.into(), service), key);
        }
    }

    /// Remove a key. No-op if it wasn't set.
    pub fn unset(&self, tenant: &str, service: ServiceId) {
        if let Ok(mut g) = self.inner.write() {
            g.remove(&(tenant.to_string(), service));
        }
    }

    /// Populate the `"dev"` tenant from environment variables:
    /// `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`. Missing vars are skipped.
    /// Use the per-tenant `set` for anything else.
    #[must_use]
    pub fn from_env() -> Self {
        let s = Self::new();
        if let Ok(v) = std::env::var("OPENAI_API_KEY") {
            s.set("dev", ServiceId::OpenAi, SecretString::new(v));
        }
        if let Ok(v) = std::env::var("ANTHROPIC_API_KEY") {
            s.set("dev", ServiceId::Anthropic, SecretString::new(v));
        }
        s
    }
}

#[async_trait]
impl KeyStore for InMemoryKeyStore {
    async fn get(&self, tenant: &str, service: ServiceId) -> Option<SecretString> {
        self.inner
            .read()
            .ok()?
            .get(&(tenant.to_string(), service))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let s = SecretString::new("sk-abcdef");
        assert_eq!(format!("{s:?}"), "<redacted>");
    }

    #[tokio::test]
    async fn in_memory_roundtrip_per_tenant() {
        let s = InMemoryKeyStore::new();
        s.set("acme",   ServiceId::OpenAi, SecretString::new("sk-acme"));
        s.set("globex", ServiceId::OpenAi, SecretString::new("sk-globex"));

        assert_eq!(s.get("acme",   ServiceId::OpenAi).await.unwrap().expose(), "sk-acme");
        assert_eq!(s.get("globex", ServiceId::OpenAi).await.unwrap().expose(), "sk-globex");
        // Cross-tenant misses are None, not a fallback to another tenant.
        assert!(s.get("acme",    ServiceId::Anthropic).await.is_none());
        assert!(s.get("unknown", ServiceId::OpenAi).await.is_none());
    }

    #[tokio::test]
    async fn unset_is_per_tenant() {
        let s = InMemoryKeyStore::new();
        s.set("acme",   ServiceId::OpenAi, SecretString::new("a"));
        s.set("globex", ServiceId::OpenAi, SecretString::new("b"));
        s.unset("acme", ServiceId::OpenAi);
        assert!(s.get("acme",   ServiceId::OpenAi).await.is_none());
        assert!(s.get("globex", ServiceId::OpenAi).await.is_some());
    }
}
