//! Central error types for the cistella driver.

use thiserror::Error;

/// Driver-wide error.
#[derive(Debug, Error)]
pub enum CistellaError {
    /// Profile validation failure.
    #[error("profile: {0}")]
    Profile(String),
    /// Mount validation failure.
    #[error("mount: {0}")]
    Mount(String),
    /// Runtime failure.
    #[error("runtime: {0}")]
    Runtime(String),
    /// Transport failure.
    #[error("transport: {0}")]
    Transport(String),
    /// Framework contract violation (spine, merge, baseline, policy shape).
    #[error("contract: {0}")]
    Contract(String),
    /// Identity failure.
    #[error("identity: {0}")]
    Identity(String),
    /// Preflight failure.
    #[error("preflight: {0}")]
    Preflight(String),
    /// Advisory lock is held by another process (non-blocking acquire).
    #[error("lock contended")]
    LockContended,
    /// Selector usage failure (ambiguous, empty, or mixed forms).
    #[error("selector: {0}")]
    Selector(String),
    /// IO wrapper.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CistellaError>;
