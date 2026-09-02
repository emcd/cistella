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
    /// Identity failure.
    #[error("identity: {0}")]
    Identity(String),
    /// Preflight failure.
    #[error("preflight: {0}")]
    Preflight(String),
    /// IO wrapper.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CistellaError>;
