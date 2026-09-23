/// Top-level error type shared by all dexter crates.
#[derive(Debug, thiserror::Error)]
pub enum DexterError {
    #[error("driver error: {0}")]
    Driver(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("target not found: {0}")]
    NotFound(String),

    #[error("ambiguous target: {0}")]
    Ambiguous(String),

    #[error("policy denied: {0}")]
    PolicyDenied(String),

    #[error("approval required: {0}")]
    ApprovalRequired(String),

    #[error("verification failed: {0}")]
    VerificationFailed(String),

    #[error("decision engine error: {0}")]
    Decision(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}
