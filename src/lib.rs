//! Cistella crate root.
//!
//! External driver for Agentmux agent development containers. See
//! `.auxiliary/agents/project.md` for project context and the
//! design-session verdicts at `home:coordination/6` for the
//! authoritative scope.

/// Returns the cistella package version as declared in `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
