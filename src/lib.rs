//! Cistella crate root.
//!
//! External driver for Agentmux agent development containers. See
//! `.auxiliary/agents/project.md` for project context and the
//! design-session verdicts at `home:coordination/6` for the
//! authoritative scope.

pub mod cli;
pub mod error;
pub mod framework;
pub mod identity;
pub mod isolators;
pub mod lock;
pub mod mount;
pub mod preflight;
pub mod prepare;
pub mod profile;
pub mod registry;
pub mod runtime;
pub mod session;
pub mod template;
pub mod transport;

/// Returns the cistella package version as declared in `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
