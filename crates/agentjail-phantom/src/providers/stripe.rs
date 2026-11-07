//! Stripe upstream.

use http::HeaderMap;

use crate::error::Result;
use crate::keys::SecretString;
use crate::provider::{Provider, ServiceId, set_bearer};

const DEFAULT_BASE: &str = "https://api.stripe.com";

/// Provider for `api.stripe.com`.
///
/// Stripe uses Basic auth semantically but accepts Bearer for the secret key
/// on every endpoint (see [Stripe API docs]). We inject `Authorization:
/// Bearer <sk_...>`.
///
/// [Stripe API docs]: https://stripe.com/docs/api/authentication
pub struct StripeProvider {
    base: String,
}

impl StripeProvider {
    /// Use the official upstream at `https://api.stripe.com`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base: DEFAULT_BASE.into(),
        }
    }

    /// Override the upstream base URL (tests only).
    #[must_use]
    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl Default for StripeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for StripeProvider {
    fn id(&self) -> ServiceId {
        ServiceId::Stripe
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
        let p = StripeProvider::new();
        let mut h = HeaderMap::new();
        p.inject_auth(&mut h, &SecretString::new("sk_live_abc"))
            .unwrap();
        assert_eq!(
            h.get(http::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer sk_live_abc"
        );
    }

    #[test]
    fn id_and_base_stable() {
        let p = StripeProvider::new();
        assert_eq!(p.id(), ServiceId::Stripe);
        assert_eq!(p.upstream_base(), "https://api.stripe.com");
    }
}
