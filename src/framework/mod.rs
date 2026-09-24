//! Framework lifecycle/capability contract (task 1.1).
//!
//! Spine phases, typed prepare-transaction merge, deadline ownership,
//! and reconciliation identity. The isolator trait (task 1.2) builds
//! on these types; nothing here knows Podman.

pub mod contract;
pub mod credentials;
pub mod guest;
pub mod isolator;
pub mod policy;
pub mod prepare;
pub mod protocol;
pub mod signals;
