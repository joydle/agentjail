//! Error types for the control plane.

/// Crate result alias.
pub type Result<T, E = CtlError> = std::result::Result<T, E>;

/// Errors surfaced by the control plane.
#[derive(Debug, thiserror::Error)]
pub enum CtlError {
    /// Bad request payload or parameter.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Resource missing.
    #[error("not found: {0}")]
    NotFound(String),

    /// Caller lacks the required API key.
    #[error("unauthorized")]
    Unauthorized,

    /// Resource conflict (e.g. duplicate).
    #[error("conflict: {0}")]
    Conflict(String),

    /// Jail execution failed.
    #[error("jail: {0}")]
    Jail(#[from] agentjail::JailError),

    /// Wraps phantom-layer error.
    #[error("phantom: {0}")]
    Phantom(#[from] agentjail_phantom::PhantomError),

    /// I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Catch-all for unexpected internal failure.
    #[error("internal: {0}")]
    Internal(String),

    /// Invalid configuration surfaced by [`crate::ControlPlaneConfig::validate`].
    #[error("config: {0}")]
    Config(String),
}

impl CtlError {
    pub(crate) fn status(&self) -> http::StatusCode {
        use http::StatusCode;
        match self {
            CtlError::BadRequest(_) => StatusCode::BAD_REQUEST,
            CtlError::NotFound(_) => StatusCode::NOT_FOUND,
            CtlError::Unauthorized => StatusCode::UNAUTHORIZED,
            CtlError::Conflict(_) => StatusCode::CONFLICT,
            CtlError::Config(_) => StatusCode::BAD_REQUEST,
            CtlError::Jail(_)
            | CtlError::Phantom(_)
            | CtlError::Io(_)
            | CtlError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}
