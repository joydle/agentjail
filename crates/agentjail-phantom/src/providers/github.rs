//! GitHub REST + GraphQL upstream.

use http::HeaderMap;

use crate::error::Result;
use crate::keys::SecretString;
use crate::provider::{Provider, ServiceId, set_bearer};

const DEFAULT_BASE: &str = "https://api.github.com";

/// Provider for `api.github.com`.
///
/// GitHub accepts `Authorization: Bearer <token>` for both fine-grained PATs
/// (`github_pat_*`), classic PATs (`ghp_*`), and installation tokens
/// (`ghs_*`). The proxy injects whatever secret the operator stored.
pub struct GitHubProvider {
    base: String,
}

impl GitHubProvider {
    /// Use the official upstream at `https://api.github.com`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base: DEFAULT_BASE.into(),
        }
    }

    /// Override the upstream base URL (GitHub Enterprise or tests).
    #[must_use]
    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl Default for GitHubProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for GitHubProvider {
    fn id(&self) -> ServiceId {
        ServiceId::GitHub
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
        let p = GitHubProvider::new();
        let mut h = HeaderMap::new();
        p.inject_auth(&mut h, &SecretString::new("ghp_abc"))
            .unwrap();
        assert_eq!(
            h.get(http::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer ghp_abc"
        );
    }

    #[test]
    fn id_and_base_stable() {
        let p = GitHubProvider::new();
        assert_eq!(p.id(), ServiceId::GitHub);
        assert_eq!(p.upstream_base(), "https://api.github.com");
    }
}
