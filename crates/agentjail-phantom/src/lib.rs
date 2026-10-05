//! # agentjail-phantom
//!
//! Phantom-token reverse proxy. The sandboxed process sees a random
//! `phm_<64hex>` token bound to a session; the proxy validates it and
//! forwards the request to the real upstream with the real key injected.
//! Real credentials never enter the jail.
//!
//! ## Shape
//!
//! ```text
//!   jail ──► HTTP  ──► phantom-proxy ──► HTTPS ──► upstream
//!         phm_<hex>                  real key
//! ```
//!
//! URLs on the proxy side look like
//! `http://proxy/v1/<service>/<upstream-path>`. Example:
//!
//! ```text
//!   POST /v1/openai/chat/completions
//!   Authorization: Bearer phm_<hex>
//! ```
//!
//! is rewritten to
//!
//! ```text
//!   POST https://api.openai.com/chat/completions
//!   Authorization: Bearer sk-<real>
//! ```
//!
//! ## Minimal example
//!
//! ```no_run
//! use std::sync::Arc;
//! use agentjail_phantom::{
//!     PhantomProxy, InMemoryTokenStore, InMemoryKeyStore, SecretString,
//!     ServiceId, Scope, TokenStore, providers::OpenAiProvider, TracingAudit,
//! };
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let tokens = Arc::new(InMemoryTokenStore::new());
//! let keys = Arc::new(InMemoryKeyStore::new());
//! keys.set("dev", ServiceId::OpenAi, SecretString::new("sk-real"));
//!
//! let proxy = PhantomProxy::builder()
//!     .provider(Arc::new(OpenAiProvider::new()))?
//!     .tokens(tokens.clone())
//!     .keys(keys)
//!     .audit(Arc::new(TracingAudit))
//!     .build()?;
//!
//! // Issue a phantom token for a session.
//! let phantom = tokens
//!     .issue("sess_demo".into(), "dev".into(), ServiceId::OpenAi, Scope::any(), None)
//!     .await;
//! println!("OPENAI_API_KEY={}", phantom.to_string());
//!
//! // Serve until ctrl-c.
//! proxy
//!     .serve(
//!         "127.0.0.1:8443".parse()?,
//!         async { let _ = tokio::signal::ctrl_c().await; },
//!     )
//!     .await?;
//! # Ok(()) }
//! ```
//!
//! ## Design notes
//!
//! - **Constant-time token compare.** `PhantomToken::ct_eq` uses `subtle`.
//! - **Redacted debug.** `PhantomToken` and `SecretString` never print.
//! - **Scope enforcement.** Tokens carry a [`Scope`] of path globs; the
//!   proxy rejects paths not in the glob list.
//! - **Streaming.** Request and response bodies are streamed both ways
//!   (OpenAI/Anthropic SSE works out of the box).
//! - **No TLS on the listen side.** This service lives on a host-local
//!   veth peer; the wire never leaves the host. `reqwest` speaks TLS
//!   to the upstream.

#![warn(missing_docs)]

mod error;
mod keys;
mod provider;
pub mod providers;
mod proxy;
mod token;

pub use error::{PhantomError, Result};
pub use keys::{InMemoryKeyStore, KeyStore, SecretString};
pub use provider::{Provider, ProviderRegistry, ServiceId};
pub use proxy::{AuditEntry, AuditSink, NoAudit, PhantomProxy, PhantomProxyBuilder, TracingAudit};
pub use token::{
    InMemoryTokenStore, LruTokenCache, PathGlob, PhantomToken, Scope, TokenRecord, TokenStore,
};
