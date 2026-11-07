//! OpenAI upstream.

use http::HeaderMap;

use crate::error::Result;
use crate::keys::SecretString;
use crate::provider::{Provider, ServiceId, set_bearer};

const DEFAULT_BASE: &str = "https://api.openai.com";

/// Provider for `api.openai.com`.
pub struct OpenAiProvider {
    base: String,
}

impl OpenAiProvider {
    /// Use the official upstream at `https://api.openai.com`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base: DEFAULT_BASE.into(),
        }
    }

    /// Override the upstream base URL (useful for tests or Azure OpenAI).
    #[must_use]
    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl Default for OpenAiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for OpenAiProvider {
    fn id(&self) -> ServiceId {
        ServiceId::OpenAi
    }

    fn upstream_base(&self) -> &str {
        &self.base
    }

    fn inject_auth(&self, headers: &mut HeaderMap, secret: &SecretString) -> Result<()> {
        set_bearer(headers, secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_bearer() {
        let p = OpenAiProvider::new();
        let mut h = HeaderMap::new();
        p.inject_auth(&mut h, &SecretString::new("sk-real"))
            .unwrap();
        let got = h.get(http::header::AUTHORIZATION).unwrap();
        assert_eq!(got.to_str().unwrap(), "Bearer sk-real");
        assert!(got.is_sensitive(), "auth header must be marked sensitive");
    }

    #[test]
    fn id_and_base_stable() {
        let p = OpenAiProvider::new();
        assert_eq!(p.id(), ServiceId::OpenAi);
        assert_eq!(p.upstream_base(), "https://api.openai.com");
    }
}
