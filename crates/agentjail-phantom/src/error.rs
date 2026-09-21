//! Error types.

use std::net::SocketAddr;

/// Crate-wide result type.
pub type Result<T, E = PhantomError> = std::result::Result<T, E>;

/// All errors surfaced by the phantom proxy.
#[derive(Debug, thiserror::Error)]
pub enum PhantomError {
    /// Failed to bind the listen socket.
    #[error("bind {addr}: {source}")]
    Bind {
        /// Address we tried to bind.
        addr: SocketAddr,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A provider was registered twice.
    #[error("duplicate provider: {0}")]
    DuplicateProvider(&'static str),

    /// No real upstream key is configured for this service.
    #[error("no key configured for service {0}")]
    MissingKey(&'static str),

    /// Configuration was invalid at construction time.
    #[error("config: {0}")]
    Config(String),

    /// Unexpected I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
