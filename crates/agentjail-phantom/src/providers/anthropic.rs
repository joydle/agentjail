//! Anthropic upstream.

use http::{HeaderMap, HeaderName, HeaderValue};

use crate::error::{PhantomError, Result};
use crate::keys::SecretString;
use crate::provider::{Provider, ServiceId, set_api_key_header};

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const DEFAULT_VERSION: &str = "2023-06-01";

/// Provider for `api.anthropic.com`.
pub struct AnthropicProvider {
    base: String,
    /// Value injected as `anthropic-version` if the client didn't send one.
    default_version: String,
}

impl AnthropicProvider {
    /// Use the official upstream.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base: DEFAULT_BASE.into(),
            default_version: DEFAULT_VERSION.into(),
        }
    }

    /// Override the upstream base URL (useful for tests).
    #[must_use]
    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            default_version: DEFAULT_VERSION.into(),
        }
    }

    /// Override the default `anthropic-version`.
    #[must_use]
    pub fn with_version(mut self, v: impl Into<String>) -> Self {
        self.default_version = v.into();
        self
    }
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for AnthropicProvider {
    fn id(&self) -> ServiceId {
        ServiceId::Anthropic
    }

    fn upstream_base(&self) -> &str {
        &self.base
    }

    fn inject_auth(&self, headers: &mut HeaderMap, secret: &SecretString) -> Result<()> {
        set_api_key_header(headers, "x-api-key", secret)?;
        let version_header = HeaderName::from_static("anthropic-version");
        if !headers.contains_key(&version_header) {
            let v = HeaderValue::from_str(&self.default_version)
                .map_err(|_| PhantomError::Config("invalid anthropic-version".into()))?;
            headers.insert(version_header, v);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_api_key_and_version() {
        let p = AnthropicProvider::new();
        let mut h = HeaderMap::new();
        p.inject_auth(&mut h, &SecretString::new("sk-ant-real"))
            .unwrap();
        assert_eq!(h.get("x-api-key").unwrap().to_str().unwrap(), "sk-ant-real");
        assert_eq!(
            h.get("anthropic-version").unwrap().to_str().unwrap(),
            "2023-06-01"
        );
    }

    #[test]
    fn preserves_client_version() {
        let p = AnthropicProvider::new();
        let mut h = HeaderMap::new();
        h.insert("anthropic-version", "2024-10-01".parse().unwrap());
        p.inject_auth(&mut h, &SecretString::new("sk-ant")).unwrap();
        assert_eq!(
            h.get("anthropic-version").unwrap().to_str().unwrap(),
            "2024-10-01"
        );
    }

    #[test]
    fn api_key_is_marked_sensitive() {
        let p = AnthropicProvider::new();
        let mut h = HeaderMap::new();
        p.inject_auth(&mut h, &SecretString::new("sk-ant")).unwrap();
        assert!(h.get("x-api-key").unwrap().is_sensitive());
    }
}
