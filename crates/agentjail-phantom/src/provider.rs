//! Upstream-service providers.
//!
//! A [`Provider`] describes one upstream (OpenAI, Anthropic, ...). Given a
//! real secret and an incoming proxy request, it produces the outbound
//! upstream URL and the headers to attach.

use std::collections::HashMap;

use http::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::error::{PhantomError, Result};
use crate::keys::SecretString;

/// Stable string identifier for a service. Part of public config / API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceId {
    /// OpenAI (api.openai.com).
    OpenAi,
    /// Anthropic (api.anthropic.com).
    Anthropic,
    /// GitHub (api.github.com).
    GitHub,
    /// Stripe (api.stripe.com).
    Stripe,
}

impl ServiceId {
    /// The stable string name used in URLs (`/v1/<name>`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ServiceId::OpenAi => "openai",
            ServiceId::Anthropic => "anthropic",
            ServiceId::GitHub => "github",
            ServiceId::Stripe => "stripe",
        }
    }
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// An upstream service definition. Stateless — instances are shared across
/// all proxy requests for that service.
pub trait Provider: Send + Sync + 'static {
    /// Stable identifier.
    fn id(&self) -> ServiceId;

    /// Base URL of the real upstream. Must be `https://`, no trailing slash.
    fn upstream_base(&self) -> &str;

    /// Mutate `headers` to carry the real secret in whatever form the upstream
    /// expects. Implementations must **not** leave the phantom token in place.
    fn inject_auth(&self, headers: &mut HeaderMap, secret: &SecretString) -> Result<()>;

    /// Remove any headers the sandbox may have set that would leak or confuse
    /// the upstream (e.g. `Host`, `Authorization`, `x-api-key`, hop-by-hop).
    fn strip_client_headers(&self, headers: &mut HeaderMap) {
        strip_standard_client_headers(headers);
    }
}

/// Headers every provider should strip from the incoming request before the
/// provider-specific `inject_auth` runs.
pub fn strip_standard_client_headers(headers: &mut HeaderMap) {
    // Auth headers — always removed; providers re-inject.
    headers.remove(http::header::AUTHORIZATION);
    headers.remove(HeaderName::from_static("x-api-key"));
    // Hop-by-hop — per RFC 7230 §6.1.
    headers.remove(http::header::CONNECTION);
    headers.remove(http::header::PROXY_AUTHENTICATE);
    headers.remove(http::header::PROXY_AUTHORIZATION);
    headers.remove(http::header::TE);
    headers.remove(http::header::TRAILER);
    headers.remove(http::header::TRANSFER_ENCODING);
    headers.remove(http::header::UPGRADE);
    // Host must be rewritten per upstream.
    headers.remove(http::header::HOST);
}

/// Registry of providers, keyed by [`ServiceId`].
#[derive(Default)]
pub struct ProviderRegistry {
    providers: HashMap<ServiceId, std::sync::Arc<dyn Provider>>,
}

impl ProviderRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider. Errors if one is already registered for that id.
    pub fn register(&mut self, provider: std::sync::Arc<dyn Provider>) -> Result<()> {
        let id = provider.id();
        if self.providers.contains_key(&id) {
            return Err(PhantomError::DuplicateProvider(id.name()));
        }
        self.providers.insert(id, provider);
        Ok(())
    }

    /// Look up a provider by id.
    #[must_use]
    pub fn get(&self, id: ServiceId) -> Option<std::sync::Arc<dyn Provider>> {
        self.providers.get(&id).cloned()
    }

    /// Find a provider by URL segment (`openai` → `ServiceId::OpenAi`).
    #[must_use]
    pub fn find_by_segment(
        &self,
        segment: &str,
    ) -> Option<(ServiceId, std::sync::Arc<dyn Provider>)> {
        self.providers
            .iter()
            .find(|(id, _)| id.name() == segment)
            .map(|(id, p)| (*id, p.clone()))
    }
}

/// Helper to set an `Authorization: Bearer <secret>` header.
pub(crate) fn set_bearer(headers: &mut HeaderMap, secret: &SecretString) -> Result<()> {
    let value = format!("Bearer {}", secret.expose());
    let v = HeaderValue::from_str(&value)
        .map_err(|_| PhantomError::Config("upstream secret not valid header".into()))?;
    // Mark sensitive so tracing / debug won't print it.
    let mut v = v;
    v.set_sensitive(true);
    headers.insert(http::header::AUTHORIZATION, v);
    Ok(())
}

/// Helper to set a `x-api-key: <secret>` header (Anthropic style).
pub(crate) fn set_api_key_header(
    headers: &mut HeaderMap,
    header: &'static str,
    secret: &SecretString,
) -> Result<()> {
    let mut v = HeaderValue::from_str(secret.expose())
        .map_err(|_| PhantomError::Config("upstream secret not valid header".into()))?;
    v.set_sensitive(true);
    headers.insert(HeaderName::from_static(header), v);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy(ServiceId);
    impl Provider for Dummy {
        fn id(&self) -> ServiceId {
            self.0
        }
        fn upstream_base(&self) -> &'static str {
            "https://example.com"
        }
        fn inject_auth(&self, _headers: &mut HeaderMap, _s: &SecretString) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn registry_rejects_duplicates() {
        let mut r = ProviderRegistry::new();
        r.register(std::sync::Arc::new(Dummy(ServiceId::OpenAi)))
            .unwrap();
        let err = r
            .register(std::sync::Arc::new(Dummy(ServiceId::OpenAi)))
            .unwrap_err();
        assert!(matches!(err, PhantomError::DuplicateProvider(_)));
    }

    #[test]
    fn registry_find_by_segment() {
        let mut r = ProviderRegistry::new();
        r.register(std::sync::Arc::new(Dummy(ServiceId::OpenAi)))
            .unwrap();
        assert!(r.find_by_segment("openai").is_some());
        assert!(r.find_by_segment("nope").is_none());
    }

    #[test]
    fn strip_removes_auth_and_hop_by_hop() {
        let mut h = HeaderMap::new();
        h.insert(http::header::AUTHORIZATION, "Bearer phm_x".parse().unwrap());
        h.insert("x-api-key", "phm_y".parse().unwrap());
        h.insert(http::header::HOST, "127.0.0.1".parse().unwrap());
        h.insert(http::header::CONNECTION, "keep-alive".parse().unwrap());
        h.insert("user-agent", "curl".parse().unwrap());

        strip_standard_client_headers(&mut h);

        assert!(h.get(http::header::AUTHORIZATION).is_none());
        assert!(h.get("x-api-key").is_none());
        assert!(h.get(http::header::HOST).is_none());
        assert!(h.get(http::header::CONNECTION).is_none());
        // Non-hop-by-hop preserved
        assert_eq!(h.get("user-agent").unwrap(), "curl");
    }

    #[test]
    fn service_id_names_are_stable() {
        assert_eq!(ServiceId::OpenAi.name(), "openai");
        assert_eq!(ServiceId::Anthropic.name(), "anthropic");
    }
}
