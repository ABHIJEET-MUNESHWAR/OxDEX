//! Top-level error type used across all OxDEX crates.

use thiserror::Error;

/// Convenience alias.
pub type Result<T> = std::result::Result<T, OxDexError>;

/// All recoverable failure modes in OxDEX.
///
/// We deliberately keep the variants coarse-grained at the crate boundary;
/// each crate may wrap a more specific error in its own module.
#[derive(Debug, Error)]
pub enum OxDexError {
    /// Address could not be parsed (wrong length, bad base58, etc.).
    #[error("invalid address: {0}")]
    InvalidAddress(String),

    /// Order failed semantic validation (e.g. zero amount, expired).
    #[error("invalid order: {0}")]
    InvalidOrder(String),

    /// Cryptographic signature failed to verify.
    #[error("signature verification failed: {0}")]
    BadSignature(String),

    /// Order/account not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// Conflict with existing state (e.g. duplicate nonce).
    #[error("conflict: {0}")]
    Conflict(String),

    /// Solver produced an inconsistent solution.
    #[error("invalid solution: {0}")]
    InvalidSolution(String),

    /// Storage / database error.
    #[error("storage error: {0}")]
    Storage(String),

    /// Network / RPC error.
    #[error("network error: {0}")]
    Network(String),

    /// Configuration error at startup.
    #[error("configuration error: {0}")]
    Config(String),

    /// Anything else; prefer specific variants when you can.
    #[error("internal error: {0}")]
    Internal(String),
}

impl OxDexError {
    /// Stable string code suitable for HTTP error bodies / logs / metrics.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidAddress(_)  => "invalid_address",
            Self::InvalidOrder(_)    => "invalid_order",
            Self::BadSignature(_)    => "bad_signature",
            Self::NotFound(_)        => "not_found",
            Self::Conflict(_)        => "conflict",
            Self::InvalidSolution(_) => "invalid_solution",
            Self::Storage(_)         => "storage_error",
            Self::Network(_)         => "network_error",
            Self::Config(_)          => "config_error",
            Self::Internal(_)        => "internal_error",
        }
    }

    /// Whether the caller can usefully retry (idempotent, transient failure).
    pub fn is_retriable(&self) -> bool {
        matches!(self, Self::Storage(_) | Self::Network(_))
    }
}

